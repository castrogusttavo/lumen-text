//! Text buffer backed by a rope, with grouped undo/redo and atomic save.
//! All positions are char indices into the rope.

use ropey::Rope;
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
};


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
}

impl Sel {
    pub fn caret(p: usize) -> Self {
        Self { anchor: p, head: p }
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Typing,
    Backspace,
    Delete,
    Other,
}

struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

struct Node {
    group: Group,
    parent: Option<usize>,
    children: Vec<usize>,
    last_child: Option<usize>, // where Redo goes
}

/// Undo history as a tree: editing after an undo starts a new branch instead of discarding
/// what was undone, and any branch can be switched back to.
#[derive(Default)]
struct History {
    nodes: Vec<Node>,
    cur: Option<usize>, // last applied node (None = original text)
    roots: Vec<usize>,
    root_last: Option<usize>,
}

impl History {
    /// The current group, only while it is a leaf (interior nodes never absorb new edits).
    fn cur_group(&self) -> Option<&Group> {
        let n = &self.nodes[self.cur?];
        n.children.is_empty().then_some(&n.group)
    }

    fn cur_group_mut(&mut self) -> Option<&mut Group> {
        let i = self.cur?;
        let n = &mut self.nodes[i];
        n.children.is_empty().then_some(&mut n.group)
    }

    fn push(&mut self, g: Group) {
        let idx = self.nodes.len();
        let parent = self.cur;
        self.nodes.push(Node { group: g, parent, children: Vec::new(), last_child: None });
        match parent {
            Some(p) => {
                self.nodes[p].children.push(idx);
                self.nodes[p].last_child = Some(idx);
            }
            None => {
                self.roots.push(idx);
                self.root_last = Some(idx);
            }
        }
        self.cur = Some(idx);
    }

    fn drop_cur_if_empty(&mut self) {
        let Some(i) = self.cur else { return };
        if !(self.nodes[i].group.edits.is_empty() && self.nodes[i].children.is_empty()) {
            return;
        }
        let parent = self.nodes[i].parent;
        let (list, last) = match parent {
            Some(p) => {
                let n = &mut self.nodes[p];
                (&mut n.children as *mut Vec<usize>, &mut n.last_child as *mut Option<usize>)
            }
            None => (&mut self.roots as *mut Vec<usize>, &mut self.root_last as *mut Option<usize>),
        };
        // SAFETY: both pointers refer to distinct live fields of `self`, used only here.
        unsafe {
            (*list).retain(|&c| c != i);
            *last = (*list).last().copied();
        }
        self.cur = parent;
    }

    fn redo_target(&self) -> Option<usize> {
        match self.cur {
            Some(c) => self.nodes[c].last_child,
            None => self.root_last,
        }
    }

    fn branches(&self) -> Vec<usize> {
        match self.cur {
            Some(c) => self.nodes[c].children.clone(),
            None => self.roots.clone(),
        }
    }
}

struct Group {
    edits: Vec<Edit>,
    before: Sel,
    after: Sel,
    kind: EditKind,
    ver_before: u64,
    ver_after: u64,
}

pub struct Buffer {
    rope: Rope,
    pub path: Option<PathBuf>,
    hist: History,
    open: bool,     // the last undo group still accepts merged edits
    compound: bool, // everything until end_compound() joins one group
    version: u64,
    counter: u64,
    saved_version: u64,
    dirty_line: Option<usize>,
    pub crlf: bool,
}

fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

impl Buffer {
    pub fn from_rope(rope: Rope, path: Option<PathBuf>) -> Self {
        let crlf = rope.len_chars() > 1
            && (0..rope.len_chars().min(100_000)).any(|i| rope.char(i) == '\r' && rope.get_char(i + 1) == Some('\n'));
        Self {
            rope,
            path,
            hist: History::default(),
            open: false,
            compound: false,
            version: 0,
            counter: 0,
            saved_version: 0,
            dirty_line: None,
            crlf,
        }
    }

    #[cfg(test)]
    pub fn from_str(s: &str) -> Self {
        Self::from_rope(Rope::from_str(s), None)
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let rope = Rope::from_reader(BufReader::new(File::open(path)?))?;
        Ok(Self::from_rope(rope, Some(path.to_path_buf())))
    }

    /// O(1) immutable copy of the text (ropey shares its nodes), for background workers.
    pub fn snapshot(&self) -> Rope {
        self.rope.clone()
    }

    pub fn eol(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    // ── queries ──

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    #[allow(dead_code)]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }
    pub fn char_at(&self, c: usize) -> Option<char> {
        self.rope.get_char(c)
    }
    pub fn line_start(&self, l: usize) -> usize {
        self.rope.line_to_char(l.min(self.line_count()))
    }
    #[allow(dead_code)]
    pub fn char_to_line(&self, c: usize) -> usize {
        self.rope.char_to_line(c.min(self.len_chars()))
    }

    /// Number of chars in the line, excluding its line break.
    pub fn line_len(&self, l: usize) -> usize {
        let s = self.rope.line(l);
        let mut n = s.len_chars();
        if n > 0 && is_break(s.char(n - 1)) {
            let last_is_lf = s.char(n - 1) == '\n';
            n -= 1;
            if last_is_lf && n > 0 && s.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        n
    }

    /// Line text without its line break.
    pub fn line(&self, l: usize) -> String {
        let n = self.line_len(l);
        self.rope.line(l).slice(..n).to_string()
    }

    pub fn slice(&self, a: usize, b: usize) -> String {
        self.rope.slice(a.min(b)..b.max(a).min(self.len_chars())).to_string()
    }

    /// (line, column) of a char index.
    pub fn line_col(&self, c: usize) -> (usize, usize) {
        let c = c.min(self.len_chars());
        let l = self.rope.char_to_line(c);
        (l, c - self.rope.line_to_char(l))
    }

    pub fn pos(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        self.line_start(line) + col.min(self.line_len(line))
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    /// Lowest line touched since the last call (for highlighter invalidation).
    pub fn take_dirty_line(&mut self) -> Option<usize> {
        self.dirty_line.take()
    }

    fn mark(&mut self, line: usize) {
        self.dirty_line = Some(self.dirty_line.map_or(line, |d| d.min(line)));
    }

    // ── editing ──

    /// Replace chars [start, end) with `text`, recording undo information.
    pub fn replace(&mut self, start: usize, end: usize, text: &str, kind: EditKind, before: Sel) {
        let (start, end) = (start.min(end), start.max(end).min(self.len_chars()));
        if start == end && text.is_empty() {
            return;
        }
        let removed = self.rope.slice(start..end).to_string();
        let line = self.rope.char_to_line(start);
        self.mark(line);

        let merge = self.compound
            || (self.open && kind != EditKind::Other && self.hist.cur_group().is_some_and(|g| g.kind == kind));
        if !merge {
            self.counter += 1;
            let g = Group { edits: Vec::new(), before, after: before, kind, ver_before: self.version, ver_after: self.version };
            self.hist.push(g);
        }
        self.open = true;

        if start < end {
            self.rope.remove(start..end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }
        self.counter += 1;
        self.version = self.counter;
        let g = self.hist.cur_group_mut().unwrap();
        g.edits.push(Edit { at: start, removed, inserted: text.to_string() });
        g.ver_after = self.version;
    }

    pub fn set_after(&mut self, sel: Sel) {
        if let Some(g) = self.hist.cur_group_mut() {
            g.after = sel;
        }
    }

    /// Stop merging further edits into the current undo group.
    pub fn close_group(&mut self) {
        if !self.compound {
            self.open = false;
        }
    }

    /// Everything until `end_compound` becomes one undo step.
    pub fn begin_compound(&mut self, before: Sel) {
        self.compound = false;
        self.open = false;
        self.counter += 1;
        self.hist.push(Group {
            edits: Vec::new(),
            before,
            after: before,
            kind: EditKind::Other,
            ver_before: self.version,
            ver_after: self.version,
        });
        self.compound = true;
        self.open = true;
    }

    /// Start a multi-cursor edit. Same-kind typing runs keep merging into one undo step.
    pub fn begin_multi(&mut self, before: Sel, kind: EditKind) {
        let mergeable = kind != EditKind::Other;
        if mergeable && self.open && !self.compound && self.hist.cur_group().is_some_and(|g| g.kind == kind) {
            self.compound = true;
            return;
        }
        self.compound = false;
        self.open = false;
        self.counter += 1;
        self.hist.push(Group { edits: Vec::new(), before, after: before, kind, ver_before: self.version, ver_after: self.version });
        self.compound = true;
        self.open = true;
    }

    pub fn end_multi(&mut self, after: Sel, kind: EditKind) {
        self.compound = false;
        self.open = kind != EditKind::Other;
        let empty = self.hist.cur_group().is_some_and(|g| g.edits.is_empty());
        if empty {
            self.hist.drop_cur_if_empty();
            self.open = false;
        } else {
            self.set_after(after);
        }
    }

    pub fn end_compound(&mut self, after: Sel) {
        self.compound = false;
        self.open = false;
        let empty = self.hist.cur_group().is_some_and(|g| g.edits.is_empty());
        if empty {
            self.hist.drop_cur_if_empty();
        } else {
            self.set_after(after);
        }
    }

    fn apply_reverse(&mut self, idx: usize) {
        for k in (0..self.hist.nodes[idx].group.edits.len()).rev() {
            let (at, n, removed) = {
                let e = &self.hist.nodes[idx].group.edits[k];
                (e.at, e.inserted.chars().count(), e.removed.clone())
            };
            let line = self.rope.char_to_line(at.min(self.len_chars()));
            self.mark(line);
            if n > 0 {
                self.rope.remove(at..at + n);
            }
            if !removed.is_empty() {
                self.rope.insert(at, &removed);
            }
        }
    }

    fn apply_forward(&mut self, idx: usize) {
        for k in 0..self.hist.nodes[idx].group.edits.len() {
            let (at, n, inserted) = {
                let e = &self.hist.nodes[idx].group.edits[k];
                (e.at, e.removed.chars().count(), e.inserted.clone())
            };
            let line = self.rope.char_to_line(at.min(self.len_chars()));
            self.mark(line);
            if n > 0 {
                self.rope.remove(at..at + n);
            }
            if !inserted.is_empty() {
                self.rope.insert(at, &inserted);
            }
        }
    }

    pub fn undo(&mut self) -> Option<Sel> {
        self.open = false;
        self.compound = false;
        let idx = self.hist.cur?;
        self.apply_reverse(idx);
        let n = &self.hist.nodes[idx];
        self.version = n.group.ver_before;
        let sel = n.group.before;
        self.hist.cur = n.parent;
        Some(sel)
    }

    pub fn redo(&mut self) -> Option<Sel> {
        self.open = false;
        self.compound = false;
        let idx = self.hist.redo_target()?;
        self.apply_forward(idx);
        self.version = self.hist.nodes[idx].group.ver_after;
        self.hist.cur = Some(idx);
        Some(self.hist.nodes[idx].group.after)
    }

    /// Alternative futures from the current point: (node id, short description).
    pub fn undo_branches(&self) -> Vec<(usize, String)> {
        self.hist
            .branches()
            .into_iter()
            .map(|i| {
                let g = &self.hist.nodes[i].group;
                let e = g.edits.first();
                let ins: String = e.map(|e| e.inserted.chars().take(40).collect()).unwrap_or_default();
                let rem: String = e.map(|e| e.removed.chars().take(40).collect()).unwrap_or_default();
                let desc = match (ins.is_empty(), rem.is_empty()) {
                    (false, true) => format!("inserir \"{}\"", ins.replace('\n', "↵")),
                    (true, false) => format!("apagar \"{}\"", rem.replace('\n', "↵")),
                    _ => format!("trocar \"{}\" por \"{}\"", rem.replace('\n', "↵"), ins.replace('\n', "↵")),
                };
                (i, format!("{desc} ({} edição(ões))", g.edits.len()))
            })
            .collect()
    }

    /// Make `node` (one of `undo_branches`) the Redo target and apply it.
    pub fn redo_branch(&mut self, node: usize) -> Option<Sel> {
        if !self.hist.branches().contains(&node) {
            return None;
        }
        match self.hist.cur {
            Some(c) => self.hist.nodes[c].last_child = Some(node),
            None => self.hist.root_last = Some(node),
        }
        self.redo()
    }

    // ── saving ──

    /// Atomic save: write a temp file next to the target, then rename over it.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::Error::other("buffer has no path"));
        };
        let tmp = path.with_extension(format!(
            "{}lumen-tmp",
            path.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
        ));
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            self.rope.write_to(&mut w)?;
            use std::io::Write;
            w.flush()?;
        }
        if let Ok(meta) = fs::metadata(&path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, &path)?;
        self.saved_version = self.version;
        Ok(())
    }

    pub fn to_string(&self) -> String {
        self.rope.to_string()
    }
}
