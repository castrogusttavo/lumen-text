//! Text buffer backed by a rope, with grouped undo/redo and atomic save.
//! All positions are char indices into the rope.

use ropey::Rope;
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
};

mod undo_file;
pub use undo_file::SavedAt;

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

/// How an edit moved the lines of the text (what folds, wrap rows and other panes follow).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditShape {
    /// Ordinary replacement of `removed` chars by `inserted` chars.
    Splice,
    /// Two adjacent blocks of whole lines exchanged places (move line up/down): the first block
    /// has `a_lines` lines / `a_chars` chars (without its trailing break), the second `b_lines` /
    /// `b_chars`, `eol` is the length of the line break between them.
    Swap { a_lines: usize, b_lines: usize, a_chars: usize, b_chars: usize, eol: usize },
}

/// One recorded text change, in the coordinates it happened in (edits are applied in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditRec {
    pub at: usize,
    pub removed: usize,
    pub inserted: usize,
    /// Line and column of `at` (before the edit; the text before `at` never changes).
    pub line: usize,
    pub col: usize,
    /// Line breaks removed / inserted.
    pub rem_lines: usize,
    pub ins_lines: usize,
    /// Nothing but the line break follows the edit point on its line.
    pub tail_empty: bool,
    pub shape: EditShape,
}

/// Line-level effect of an edit: `after` is the last line that keeps its place; lines
/// `after+1 ..= after+n` disappear (`Remove`) or `n` new lines appear right after it (`Insert`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineOp {
    Remove { after: isize, n: usize },
    Insert { after: isize, n: usize },
    Swap { first: usize, a_lines: usize, b_lines: usize },
}

impl EditRec {
    /// Line operations, in the order they apply. The line a cut/paste at column 0 leaves behind is
    /// the one that held the text after the edit, so a header the user pushes down with Enter keeps
    /// its fold; a mid-line edit (or one that leaves nothing of the line after it) keeps the line it
    /// started in.
    pub fn line_ops(&self) -> Vec<LineOp> {
        if let EditShape::Swap { a_lines, b_lines, .. } = self.shape {
            return vec![LineOp::Swap { first: self.line, a_lines, b_lines }];
        }
        let after = if self.col > 0 || self.tail_empty { self.line as isize } else { self.line as isize - 1 };
        let mut v = Vec::new();
        if self.rem_lines > 0 {
            v.push(LineOp::Remove { after, n: self.rem_lines });
        }
        if self.ins_lines > 0 {
            v.push(LineOp::Insert { after, n: self.ins_lines });
        }
        v
    }

    /// Where char position `p` (before the edit) ends up. Positions inside removed text collapse
    /// to the edit point; a position exactly at an insertion stays before the new text.
    pub fn map_pos(&self, p: usize) -> usize {
        match self.shape {
            EditShape::Splice => {
                if p <= self.at {
                    p
                } else if p >= self.at + self.removed {
                    p + self.inserted - self.removed
                } else {
                    self.at
                }
            }
            EditShape::Swap { a_chars, b_chars, eol, .. } => {
                let a0 = self.at;
                let b0 = a0 + a_chars + eol;
                if p < a0 || p > b0 + b_chars {
                    p
                } else if p < a0 + a_chars {
                    p + b_chars + eol // inside the first block: it now comes after the second
                } else if p >= b0 {
                    p - a_chars - eol
                } else {
                    a0 + b_chars + (p - (a0 + a_chars)) // in the break between them
                }
            }
        }
    }

    /// Where line `l` (before the edit) is afterwards.
    pub fn map_line(&self, l: usize) -> usize {
        let mut l = l as isize;
        for op in self.line_ops() {
            l = match op {
                LineOp::Remove { after, n } => {
                    if l > after + n as isize {
                        l - n as isize
                    } else if l > after {
                        after.max(0)
                    } else {
                        l
                    }
                }
                LineOp::Insert { after, n } => {
                    if l > after {
                        l + n as isize
                    } else {
                        l
                    }
                }
                LineOp::Swap { first, a_lines, b_lines } => {
                    let u = l as usize;
                    if u >= first && u < first + a_lines {
                        (u + b_lines) as isize
                    } else if u >= first + a_lines && u < first + a_lines + b_lines {
                        (u - a_lines) as isize
                    } else {
                        l
                    }
                }
            };
        }
        l.max(0) as usize
    }
}

static NEXT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Edit {
    at: usize,
    removed: String,
    inserted: String,
    shape: EditShape,
}

impl EditShape {
    /// The shape of the edit that undoes this one.
    fn reversed(self) -> EditShape {
        match self {
            EditShape::Splice => EditShape::Splice,
            EditShape::Swap { a_lines, b_lines, a_chars, b_chars, eol } => {
                EditShape::Swap { a_lines: b_lines, b_lines: a_lines, a_chars: b_chars, b_chars: a_chars, eol }
            }
        }
    }
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
    /// Unique id of this buffer (a reloaded document gets a new one).
    gen_id: u64,
    /// Edits not yet consumed by the document (`take_edits`).
    pending: Vec<EditRec>,
    /// Edits kept for other panes that show this text (only while `log_on`).
    log: Vec<EditRec>,
    log_base: u64,
    log_on: bool,
    /// Shape the next edit will be recorded with.
    next_shape: Option<EditShape>,
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
            gen_id: NEXT_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            pending: Vec::new(),
            log: Vec::new(),
            log_base: 0,
            log_on: false,
            next_shape: None,
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

    /// Changes whenever the text changes (also back and forth with undo/redo).
    pub fn version(&self) -> u64 {
        self.version
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

    /// Edits since the last call, oldest first.
    pub fn take_edits(&mut self) -> Vec<EditRec> {
        std::mem::take(&mut self.pending)
    }

    pub fn gen_id(&self) -> u64 {
        self.gen_id
    }

    /// Start keeping every edit for panes that show this text.
    pub fn log_start(&mut self) {
        self.log_on = true;
    }

    /// Number of edits recorded so far (a pane remembers it to ask for what came later).
    pub fn log_seq(&self) -> u64 {
        self.log_base + self.log.len() as u64
    }

    /// Edits recorded after `seen`; `None` when they are no longer kept.
    pub fn edits_since(&self, seen: u64) -> Option<&[EditRec]> {
        let i = seen.checked_sub(self.log_base)? as usize;
        self.log.get(i..)
    }

    /// Forget edits before `upto` (every pane has seen them); `None` stops logging altogether.
    pub fn log_trim(&mut self, upto: Option<u64>) {
        match upto {
            Some(u) => {
                let k = (u.saturating_sub(self.log_base) as usize).min(self.log.len());
                self.log.drain(..k);
                self.log_base += k as u64;
            }
            None => {
                self.log_base += self.log.len() as u64;
                self.log.clear();
                self.log_on = false;
            }
        }
    }

    /// The next edit exchanges two blocks of whole lines (see [`EditShape::Swap`]).
    pub fn tag_swap(&mut self, a_lines: usize, b_lines: usize, a_chars: usize, b_chars: usize, eol: usize) {
        self.next_shape = Some(EditShape::Swap { a_lines, b_lines, a_chars, b_chars, eol });
    }

    /// Remove `n_rem` chars at `at`, insert `text`, and record what happened to the lines.
    fn splice(&mut self, at: usize, n_rem: usize, text: &str, shape: EditShape) {
        let line = self.rope.char_to_line(at);
        let col = at - self.rope.line_to_char(line);
        let n0 = self.rope.len_lines();
        if n_rem > 0 {
            self.rope.remove(at..at + n_rem);
        }
        let n1 = self.rope.len_lines();
        let tail_empty = self.rope.get_char(at).is_none_or(is_break);
        let n_ins = text.chars().count();
        if n_ins > 0 {
            self.rope.insert(at, text);
        }
        let n2 = self.rope.len_lines();
        let rec = EditRec {
            at,
            removed: n_rem,
            inserted: n_ins,
            line,
            col,
            rem_lines: n0.saturating_sub(n1),
            ins_lines: n2.saturating_sub(n1),
            tail_empty,
            shape,
        };
        if self.log_on {
            self.log.push(rec);
        }
        self.pending.push(rec);
    }

    // ── editing ──

    /// Replace chars [start, end) with `text`, recording undo information.
    pub fn replace(&mut self, start: usize, end: usize, text: &str, kind: EditKind, before: Sel) {
        let (start, end) = (start.min(end), start.max(end).min(self.len_chars()));
        if start == end && text.is_empty() {
            self.next_shape = None;
            return;
        }
        let removed = self.rope.slice(start..end).to_string();
        let line = self.rope.char_to_line(start);
        self.mark(line);

        let merge = self.compound
            || (self.open && kind != EditKind::Other && self.hist.cur_group().is_some_and(|g| g.kind == kind));
        if !merge {
            self.counter += 1;
            let g = Group {
                edits: Vec::new(),
                before,
                after: before,
                kind,
                ver_before: self.version,
                ver_after: self.version,
            };
            self.hist.push(g);
        }
        self.open = true;

        let shape = self.next_shape.take().unwrap_or(EditShape::Splice);
        self.splice(start, end - start, text, shape);
        self.counter += 1;
        self.version = self.counter;
        let g = self.hist.cur_group_mut().unwrap();
        g.edits.push(Edit { at: start, removed, inserted: text.to_string(), shape });
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
        self.hist.push(Group {
            edits: Vec::new(),
            before,
            after: before,
            kind,
            ver_before: self.version,
            ver_after: self.version,
        });
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

    /// Where the undo history stands now, for [`squash_since`](Self::squash_since).
    pub fn undo_mark(&self) -> Option<usize> {
        self.hist.cur
    }

    /// Join every undo step made since `mark` (a straight run of new steps, e.g. a replayed macro)
    /// into one. Nothing changes when the history left that run (an undo past the mark, a branch).
    pub fn squash_since(&mut self, mark: Option<usize>) {
        self.open = false;
        self.compound = false;
        let mut path = Vec::new();
        let mut c = self.hist.cur;
        while c != mark {
            let Some(i) = c else { return };
            path.push(i);
            c = self.hist.nodes[i].parent;
        }
        if path.len() < 2 {
            return;
        }
        path.reverse();
        // A straight run: each step has exactly the next one as its only child.
        let straight = path.windows(2).all(|w| self.hist.nodes[w[0]].children == [w[1]])
            && self.hist.nodes[path[path.len() - 1]].children.is_empty();
        if !straight {
            return;
        }
        let first = path[0];
        let last = path[path.len() - 1];
        let (after, ver_after) = (self.hist.nodes[last].group.after, self.hist.nodes[last].group.ver_after);
        let mut edits = Vec::new();
        for &i in &path[1..] {
            edits.append(&mut self.hist.nodes[i].group.edits);
        }
        let n = &mut self.hist.nodes[first];
        n.group.edits.append(&mut edits);
        n.group.after = after;
        n.group.ver_after = ver_after;
        n.group.kind = EditKind::Other;
        n.children.clear();
        n.last_child = None;
        // The emptied steps stay in `nodes` but nothing links to them any more.
        self.hist.cur = Some(first);
    }

    fn apply_reverse(&mut self, idx: usize) {
        for k in (0..self.hist.nodes[idx].group.edits.len()).rev() {
            let (at, n, removed, shape) = {
                let e = &self.hist.nodes[idx].group.edits[k];
                (e.at, e.inserted.chars().count(), e.removed.clone(), e.shape.reversed())
            };
            let line = self.rope.char_to_line(at.min(self.len_chars()));
            self.mark(line);
            self.splice(at, n, &removed, shape);
        }
    }

    fn apply_forward(&mut self, idx: usize) {
        for k in 0..self.hist.nodes[idx].group.edits.len() {
            let (at, n, inserted, shape) = {
                let e = &self.hist.nodes[idx].group.edits[k];
                (e.at, e.removed.chars().count(), e.inserted.clone(), e.shape)
            };
            let line = self.rope.char_to_line(at.min(self.len_chars()));
            self.mark(line);
            self.splice(at, n, &inserted, shape);
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
                    (true, false) => format!("delete \"{}\"", rem.replace('\n', "↵")),
                    _ => format!("trocar \"{}\" por \"{}\"", rem.replace('\n', "↵"), ins.replace('\n', "↵")),
                };
                (i, format!("{desc} ({} edit(s))", g.edits.len()))
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
}

impl std::fmt::Display for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod edit_log_tests {
    use super::*;

    fn rep(b: &mut Buffer, s: usize, e: usize, t: &str) {
        b.replace(s, e, t, EditKind::Other, Sel::caret(0));
    }

    #[test]
    fn records_describe_lines_and_positions() {
        let mut b = Buffer::from_str("ab\ncd\nef");
        rep(&mut b, 4, 4, "X\nY\n"); // in the middle of "cd"
        let r = b.take_edits();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].line, r[0].col, r[0].rem_lines, r[0].ins_lines), (1, 1, 0, 2));
        assert_eq!(r[0].line_ops(), vec![LineOp::Insert { after: 1, n: 2 }]);
        // removal across lines: from (0,1) to (2,1)
        let mut b = Buffer::from_str("ab\ncd\nef");
        rep(&mut b, 1, 7, "");
        let r = b.take_edits();
        assert_eq!((r[0].rem_lines, r[0].ins_lines), (2, 0));
        assert_eq!(r[0].line_ops(), vec![LineOp::Remove { after: 0, n: 2 }]);
        // at column 0 the line left behind is the one after the edit
        let mut b = Buffer::from_str("ab\ncd\nef");
        rep(&mut b, 3, 6, "");
        let r = b.take_edits();
        assert_eq!(r[0].line_ops(), vec![LineOp::Remove { after: 0, n: 1 }]);
        rep(&mut b, 0, 0, "new\n");
        let r = b.take_edits();
        assert_eq!(r[0].line_ops(), vec![LineOp::Insert { after: -1, n: 1 }]);
        assert!(b.take_edits().is_empty(), "drained");
    }

    #[test]
    fn positions_and_lines_map_through_an_edit() {
        let r = EditRec {
            at: 10,
            removed: 4,
            inserted: 1,
            line: 2,
            col: 0,
            rem_lines: 0,
            ins_lines: 0,
            tail_empty: false,
            shape: EditShape::Splice,
        };
        assert_eq!((r.map_pos(5), r.map_pos(10), r.map_pos(12), r.map_pos(14), r.map_pos(20)), (5, 10, 10, 11, 17));
        let ins = EditRec {
            at: 3,
            removed: 0,
            inserted: 4,
            line: 0,
            col: 3,
            rem_lines: 0,
            ins_lines: 2,
            tail_empty: false,
            shape: EditShape::Splice,
        };
        assert_eq!((ins.map_line(0), ins.map_line(1), ins.map_line(5)), (0, 3, 7));
        let del = EditRec {
            at: 0,
            removed: 6,
            inserted: 0,
            line: 0,
            col: 0,
            rem_lines: 2,
            ins_lines: 0,
            tail_empty: false,
            shape: EditShape::Splice,
        };
        assert_eq!((del.map_line(0), del.map_line(1), del.map_line(2), del.map_line(5)), (0, 0, 0, 3));
    }

    #[test]
    fn swap_records_move_whole_lines_and_undo_reverses_them() {
        let mut b = Buffer::from_str("aa\nbbb\nc\nd");
        // swap line 0 (block A) with lines 1..=2 (block B)
        b.tag_swap(1, 2, 2, 5, 1);
        rep(&mut b, 0, 8, "bbb\nc\naa");
        assert_eq!(b.to_string(), "bbb\nc\naa\nd");
        let r = b.take_edits();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].line_ops(), vec![LineOp::Swap { first: 0, a_lines: 1, b_lines: 2 }]);
        assert_eq!((r[0].map_line(0), r[0].map_line(1), r[0].map_line(2), r[0].map_line(3)), (2, 0, 1, 3));
        // chars: "aa" (0..2) -> after "bbb\nc\n"; "bbb" -> start; the break after "aa" -> after "bbb\nc"
        assert_eq!(
            (r[0].map_pos(0), r[0].map_pos(1), r[0].map_pos(3), r[0].map_pos(5), r[0].map_pos(9)),
            (6, 7, 0, 2, 9)
        );
        b.undo();
        assert_eq!(b.to_string(), "aa\nbbb\nc\nd");
        let u = b.take_edits();
        assert_eq!(
            u[0].line_ops(),
            vec![LineOp::Swap { first: 0, a_lines: 2, b_lines: 1 }],
            "undo swaps the blocks back"
        );
        assert_eq!(u[0].map_line(0), 1);
        b.redo();
        assert_eq!(b.take_edits()[0].line_ops(), vec![LineOp::Swap { first: 0, a_lines: 1, b_lines: 2 }]);
    }

    #[test]
    fn log_is_kept_only_while_asked_and_trims() {
        let mut b = Buffer::from_str("x");
        rep(&mut b, 0, 0, "a");
        assert_eq!(b.log_seq(), 0, "nothing is logged until a pane asks");
        b.log_start();
        rep(&mut b, 0, 0, "b");
        rep(&mut b, 0, 0, "c");
        assert_eq!(b.log_seq(), 2);
        assert_eq!(b.edits_since(1).map(|r| r.len()), Some(1));
        b.log_trim(Some(1));
        assert!(b.edits_since(0).is_none());
        assert_eq!(b.edits_since(1).map(|r| r.len()), Some(1));
        b.log_trim(None);
        assert!(b.edits_since(1).is_none());
        rep(&mut b, 0, 0, "d");
        assert_eq!(b.log_seq(), 2, "logging stopped");
    }
}

#[cfg(test)]
mod squash_tests {
    use super::*;

    #[test]
    fn squash_joins_the_steps_since_the_mark_into_one() {
        let mut b = Buffer::from_str("abc");
        b.replace(0, 0, "1", EditKind::Other, Sel::caret(0));
        b.close_group();
        let mark = b.undo_mark();
        // Separate steps, the way replayed keys make them.
        b.replace(4, 4, "x", EditKind::Typing, Sel::caret(4));
        b.close_group();
        b.begin_compound(Sel::caret(5));
        b.replace(0, 1, "", EditKind::Other, Sel::caret(5));
        b.end_compound(Sel::caret(4));
        b.replace(1, 2, "B", EditKind::Other, Sel::caret(4));
        b.set_after(Sel::caret(2));
        assert_eq!(b.to_string(), "aBcx");
        b.squash_since(mark);
        assert_eq!(b.undo(), Some(Sel::caret(4)), "one step back, to the selection before the first");
        assert_eq!(b.to_string(), "1abc");
        assert_eq!(b.redo(), Some(Sel::caret(2)));
        assert_eq!(b.to_string(), "aBcx");
        b.undo();
        b.undo();
        assert_eq!(b.to_string(), "abc");
        assert!(!b.is_dirty());
        // A new edit after the squashed step is a step of its own.
        b.redo();
        b.redo();
        b.replace(0, 0, "z", EditKind::Typing, Sel::caret(0));
        b.undo();
        assert_eq!(b.to_string(), "aBcx");
    }

    #[test]
    fn squash_leaves_history_alone_when_it_left_the_run() {
        let mut b = Buffer::from_str("abc");
        b.replace(0, 0, "1", EditKind::Other, Sel::caret(0));
        b.close_group();
        let mark = b.undo_mark();
        b.undo(); // back before the mark, then a new branch
        b.replace(0, 0, "2", EditKind::Other, Sel::caret(0));
        b.close_group();
        b.replace(0, 0, "3", EditKind::Other, Sel::caret(0));
        b.squash_since(mark);
        b.undo();
        assert_eq!(b.to_string(), "2abc", "still two steps");
    }
}
