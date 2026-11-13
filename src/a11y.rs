//! Screen reader support, the pure half: a [`Snapshot`] of what is on screen (plain owned data)
//! and the functions that turn it into an AccessKit tree ([`build`]) and into spoken
//! announcements ([`announcement`]). Nothing here touches the window or the app; `app_a11y`
//! fills the snapshot and hands the tree to the platform adapter, and only while an assistive
//! technology is connected, so a session without one never builds any of this.
//!
//! The tree is lean on purpose: the window, a tab list, the editor as a multi-line text input
//! (one text run per line of the visible window, with the caret and selection), the explorer as
//! a tree, the palette / pickers / prompts as a dialog with a list, the find bar, the terminal
//! as text rows, and two live regions (the status toast and the announcer).

use accesskit::{Live, Node, NodeId, Role, TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate};

pub const ROOT: NodeId = NodeId(1);
pub const TABS: NodeId = NodeId(2);
pub const FILES: NodeId = NodeId(3);
pub const EDITOR: NodeId = NodeId(4);
pub const FIND: NodeId = NodeId(5);
pub const FIND_INPUT: NodeId = NodeId(6);
pub const REPLACE_INPUT: NodeId = NodeId(7);
pub const DIALOG: NodeId = NodeId(8);
pub const DIALOG_INPUT: NodeId = NodeId(9);
pub const DIALOG_LIST: NodeId = NodeId(10);
pub const TERMINAL: NodeId = NodeId(11);
pub const TOAST: NodeId = NodeId(12);
pub const ANNOUNCER: NodeId = NodeId(13);

const TAB_BASE: u64 = 1_000;
const FILE_BASE: u64 = 10_000;
const ITEM_BASE: u64 = 20_000;
const TERM_BASE: u64 = 30_000;
const LINE_BASE: u64 = 1_000_000;

/// Most explorer rows, dialog items and terminal rows put in the tree (windows around the focus).
pub const MAX_FILES: usize = 200;
pub const MAX_ITEMS: usize = 50;
pub const MAX_TERM_ROWS: usize = 200;
/// Lines of the editor kept around the viewport, and the longest line text exposed (in chars).
pub const MAX_EDITOR_LINES: usize = 400;
pub const MAX_LINE_CHARS: usize = 1000;

/// Where the keyboard is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Area {
    #[default]
    Editor,
    Files,
    Terminal,
    Find,
    /// A command palette / picker (its list has the focus).
    Palette,
    /// A prompt or a modal window (settings, menus).
    Dialog,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct TabView {
    pub name: String,
    pub dirty: bool,
    pub active: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct EditorView {
    pub name: String,
    pub read_only: bool,
    pub line_count: usize,
    /// Number (0-based) of the first line in `lines`.
    pub first: usize,
    /// The lines of the window, without line breaks.
    pub lines: Vec<String>,
    /// Caret and selection anchor as (line, column in chars).
    pub caret: (usize, usize),
    pub anchor: (usize, usize),
    /// The diagnostic under the caret, already worded ("error: ...").
    pub diag: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct FileRow {
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub selected: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct FindView {
    pub query: String,
    pub replace: Option<String>,
    pub matches: usize,
    pub current: Option<usize>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct DialogItem {
    pub label: String,
    pub detail: String,
}

/// A modal surface: palette / picker (input + list), prompt (input only) or a plain window.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DialogView {
    pub title: String,
    pub input: Option<String>,
    pub items: Vec<DialogItem>,
    /// Selected item (index into `items`).
    pub sel: Option<usize>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct TermView {
    pub title: String,
    pub rows: Vec<String>,
    /// Cursor as (row, column in chars).
    pub cursor: (usize, usize),
}

/// Everything the tree is made of, as plain data. Equal snapshots build equal trees, so the app
/// only sends an update when the snapshot changed.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub title: String,
    pub area: Area,
    pub tabs: Vec<TabView>,
    pub editor: Option<EditorView>,
    pub files: Vec<FileRow>,
    /// Row of `files` with the explorer's cursor.
    pub files_sel: Option<usize>,
    pub find: Option<FindView>,
    pub dialog: Option<DialogView>,
    pub terminal: Option<TermView>,
    pub toast: Option<String>,
    pub recording: bool,
}

// ── windows over long lists ──────────────────────────────────────────────────────────────────

/// Lines of the editor to expose: the viewport (`first`, `visible`) plus a margin, widened to
/// hold the caret and the selection anchor, and never longer than [`MAX_EDITOR_LINES`] (the
/// caret side wins). Half-open, within `0..line_count`.
pub fn editor_window(line_count: usize, first: usize, visible: usize, caret: usize, anchor: usize) -> (usize, usize) {
    let n = line_count.max(1);
    let mut start = first.min(n - 1).saturating_sub(2);
    let mut end = (first + visible + 2).min(n);
    let caret = caret.min(n - 1);
    let anchor = anchor.min(n - 1);
    start = start.min(caret).min(anchor);
    end = end.max(caret + 1).max(anchor + 1);
    if end - start > MAX_EDITOR_LINES {
        // Too long (a huge selection): keep the caret's side.
        if caret >= anchor {
            start = end.saturating_sub(MAX_EDITOR_LINES).max(start);
            start = start.min(caret);
            end = (start + MAX_EDITOR_LINES).max(caret + 1).min(n);
        } else {
            end = (start + MAX_EDITOR_LINES).min(n).max(caret + 1);
        }
    }
    (start, end)
}

/// A window of at most `max` items around `sel` in a list of `len`: half-open range.
pub fn list_window(len: usize, sel: usize, max: usize) -> (usize, usize) {
    if len <= max {
        return (0, len);
    }
    let start = sel.min(len - 1).saturating_sub(max / 4).min(len - max);
    (start, start + max)
}

// ── building the tree ────────────────────────────────────────────────────────────────────────

fn id(base: u64, i: usize) -> NodeId {
    NodeId(base + i as u64)
}

/// The text run of one line: its text (plus the line break when `newline`), the byte length of
/// each char and where each word starts (chars, only while that fits a byte).
fn text_run(text: &str, newline: bool) -> Node {
    let mut n = Node::new(Role::TextRun);
    let mut value = String::with_capacity(text.len() + 1);
    value.push_str(text);
    if newline {
        value.push('\n');
    }
    let lengths: Vec<u8> = value.chars().map(|c| c.len_utf8() as u8).collect();
    let mut starts: Vec<u8> = Vec::new();
    let mut prev_space = true;
    for (i, c) in value.chars().enumerate() {
        let space = c.is_whitespace();
        if prev_space && !space && i <= u8::MAX as usize {
            starts.push(i as u8);
        }
        prev_space = space;
    }
    n.set_value(value);
    n.set_character_lengths(lengths);
    n.set_word_starts(starts);
    n
}

/// A position inside the exposed lines: lines outside the window clamp to its edges.
fn position(e: &EditorView, line: usize, col: usize) -> TextPosition {
    let last = e.first + e.lines.len().saturating_sub(1);
    let (l, c) = if line < e.first {
        (e.first, 0)
    } else if line > last {
        (last, usize::MAX)
    } else {
        (line, col)
    };
    let chars = e.lines.get(l - e.first).map_or(0, |t| t.chars().count());
    TextPosition { node: id(LINE_BASE, l), character_index: c.min(chars) }
}

fn tab_label(t: &TabView) -> String {
    if t.dirty { format!("{}, modified", t.name) } else { t.name.clone() }
}

fn push_text_rows(
    nodes: &mut Vec<(NodeId, Node)>,
    lines: &[String],
    base: u64,
    first: usize,
    total: usize,
) -> Vec<NodeId> {
    let mut ids = Vec::with_capacity(lines.len());
    for (i, text) in lines.iter().enumerate() {
        let line = first + i;
        let nid = id(base, line);
        nodes.push((nid, text_run(text, line + 1 < total)));
        ids.push(nid);
    }
    ids
}

/// The whole accessibility tree for `s`. `said` is the text of the announcer live region.
pub fn build(s: &Snapshot, said: &str) -> TreeUpdate {
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();
    let mut kids: Vec<NodeId> = Vec::new();
    let mut focus = ROOT;

    if !s.tabs.is_empty() {
        let mut list = Node::new(Role::TabList);
        list.set_label("Open files");
        let mut ids = Vec::with_capacity(s.tabs.len());
        for (i, t) in s.tabs.iter().enumerate() {
            let mut n = Node::new(Role::Tab);
            n.set_label(tab_label(t));
            n.set_selected(t.active);
            nodes.push((id(TAB_BASE, i), n));
            ids.push(id(TAB_BASE, i));
        }
        list.set_children(ids);
        nodes.push((TABS, list));
        kids.push(TABS);
    }

    if !s.files.is_empty() {
        let mut tree = Node::new(Role::Tree);
        tree.set_label("Files");
        let mut ids = Vec::with_capacity(s.files.len());
        for (i, f) in s.files.iter().enumerate() {
            let mut n = Node::new(Role::TreeItem);
            n.set_label(f.name.clone());
            n.set_level(f.depth + 1);
            if f.is_dir {
                n.set_expanded(f.expanded);
                n.set_description("folder");
            }
            n.set_selected(f.selected);
            nodes.push((id(FILE_BASE, i), n));
            ids.push(id(FILE_BASE, i));
        }
        if let Some(sel) = s.files_sel.filter(|i| *i < s.files.len())
            && s.area == Area::Files
        {
            focus = id(FILE_BASE, sel);
        }
        tree.set_children(ids);
        nodes.push((FILES, tree));
        kids.push(FILES);
    }

    if let Some(e) = &s.editor {
        let mut e = e.clone();
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        let ids = push_text_rows(&mut nodes, &e.lines, LINE_BASE, e.first, e.line_count);
        let mut ed = Node::new(Role::MultilineTextInput);
        ed.set_label(e.name.clone());
        if e.read_only {
            ed.set_read_only();
        }
        ed.set_text_selection(TextSelection {
            anchor: position(&e, e.anchor.0, e.anchor.1),
            focus: position(&e, e.caret.0, e.caret.1),
        });
        ed.set_children(ids);
        nodes.push((EDITOR, ed));
        kids.push(EDITOR);
        if s.area == Area::Editor {
            focus = EDITOR;
        }
    }

    if let Some(f) = &s.find {
        let mut group = Node::new(Role::Group);
        group.set_label("Find");
        let summary = match (f.matches, f.current) {
            (0, _) => "no matches".to_string(),
            (n, Some(c)) => format!("match {} of {n}", c + 1),
            (n, None) => format!("{n} matches"),
        };
        group.set_description(summary);
        let mut input = Node::new(Role::TextInput);
        input.set_label("Find");
        input.set_value(f.query.clone());
        nodes.push((FIND_INPUT, input));
        let mut ids = vec![FIND_INPUT];
        if let Some(r) = &f.replace {
            let mut input = Node::new(Role::TextInput);
            input.set_label("Replace");
            input.set_value(r.clone());
            nodes.push((REPLACE_INPUT, input));
            ids.push(REPLACE_INPUT);
        }
        group.set_children(ids);
        nodes.push((FIND, group));
        kids.push(FIND);
        if s.area == Area::Find {
            focus = FIND_INPUT;
        }
    }

    if let Some(d) = &s.dialog {
        let mut dlg = Node::new(Role::Dialog);
        dlg.set_label(d.title.clone());
        dlg.set_modal();
        let mut ids = Vec::new();
        if let Some(text) = &d.input {
            let mut input = Node::new(Role::TextInput);
            input.set_label(d.title.clone());
            input.set_value(text.clone());
            nodes.push((DIALOG_INPUT, input));
            ids.push(DIALOG_INPUT);
            focus = DIALOG_INPUT;
        } else {
            focus = DIALOG;
        }
        if !d.items.is_empty() {
            let mut list = Node::new(Role::ListBox);
            list.set_label("Results");
            let mut items = Vec::with_capacity(d.items.len());
            for (i, it) in d.items.iter().enumerate() {
                let mut n = Node::new(Role::ListBoxOption);
                n.set_label(it.label.clone());
                if !it.detail.is_empty() {
                    n.set_description(it.detail.clone());
                }
                n.set_selected(d.sel == Some(i));
                n.set_size_of_set(d.items.len());
                n.set_position_in_set(i + 1);
                nodes.push((id(ITEM_BASE, i), n));
                items.push(id(ITEM_BASE, i));
            }
            if let Some(sel) = d.sel.filter(|i| *i < d.items.len()) {
                list.set_active_descendant(id(ITEM_BASE, sel));
                if s.area == Area::Palette {
                    focus = id(ITEM_BASE, sel);
                }
            }
            list.set_children(items);
            nodes.push((DIALOG_LIST, list));
            ids.push(DIALOG_LIST);
        }
        dlg.set_children(ids);
        nodes.push((DIALOG, dlg));
        kids.push(DIALOG);
    }

    if let Some(t) = &s.terminal {
        let rows: Vec<String> = t.rows.iter().take(MAX_TERM_ROWS).cloned().collect();
        let ids = push_text_rows(&mut nodes, &rows, TERM_BASE, 0, rows.len());
        let mut term = Node::new(Role::Terminal);
        term.set_label(t.title.clone());
        if !rows.is_empty() {
            let row = t.cursor.0.min(rows.len() - 1);
            let col = t.cursor.1.min(rows[row].chars().count());
            let at = TextPosition { node: id(TERM_BASE, row), character_index: col };
            term.set_text_selection(TextSelection { anchor: at, focus: at });
        }
        term.set_children(ids);
        nodes.push((TERMINAL, term));
        kids.push(TERMINAL);
        if s.area == Area::Terminal {
            focus = TERMINAL;
        }
    }

    // Live regions stay in the tree even when empty: a region that appears already filled is
    // not announced by every screen reader.
    let mut toast = Node::new(Role::Status);
    toast.set_live(Live::Polite);
    toast.set_live_atomic();
    toast.set_label(s.toast.clone().unwrap_or_default());
    nodes.push((TOAST, toast));
    kids.push(TOAST);
    let mut announcer = Node::new(Role::Status);
    announcer.set_live(Live::Polite);
    announcer.set_live_atomic();
    announcer.set_label(said);
    nodes.push((ANNOUNCER, announcer));
    kids.push(ANNOUNCER);

    let mut root = Node::new(Role::Window);
    root.set_label(s.title.clone());
    root.set_children(kids);
    nodes.push((ROOT, root));

    let mut info = TreeInfo::new(ROOT);
    info.toolkit_name = Some("lumen-text".to_string());
    TreeUpdate { nodes, tree: Some(info), tree_id: TreeId::ROOT, focus }
}

// ── announcements ────────────────────────────────────────────────────────────────────────────

fn area_name(s: &Snapshot) -> String {
    match s.area {
        Area::Editor => match &s.editor {
            Some(e) => format!("Editor, {}", e.name),
            None => "Editor".to_string(),
        },
        Area::Files => "File explorer".to_string(),
        Area::Terminal => s.terminal.as_ref().map_or("Terminal".to_string(), |t| format!("Terminal, {}", t.title)),
        Area::Find => "Find".to_string(),
        Area::Palette | Area::Dialog => s.dialog.as_ref().map_or("Dialog".to_string(), |d| d.title.clone()),
    }
}

/// "Line 12 of 340", with the diagnostic under the caret when there is one.
fn caret_words(e: &EditorView) -> String {
    let mut out = format!("Line {} of {}", e.caret.0 + 1, e.line_count.max(1));
    if let Some(d) = &e.diag {
        out.push_str(". ");
        out.push_str(d);
    }
    out
}

/// What to say after the screen went from `prev` to `cur` (empty: nothing). The text of the line
/// and the caret movement are the screen reader's job (the tree carries the text and the caret);
/// this adds what it cannot know: which area took the keyboard, the line number, the problem
/// under the caret and the macro recorder.
pub fn announcement(prev: Option<&Snapshot>, cur: &Snapshot) -> String {
    let Some(prev) = prev else { return String::new() };
    let mut parts: Vec<String> = Vec::new();
    let editor_changed = prev.editor.as_ref().map(|e| &e.name) != cur.editor.as_ref().map(|e| &e.name);
    let title = |s: &Snapshot| s.dialog.as_ref().map(|d| d.title.clone());
    let modal = matches!(cur.area, Area::Palette | Area::Dialog);
    if prev.area != cur.area || (modal && title(prev) != title(cur)) || (editor_changed && cur.area == Area::Editor) {
        parts.push(area_name(cur));
    }
    if let (Some(a), Some(b)) = (&prev.editor, &cur.editor)
        && cur.area == Area::Editor
        && !editor_changed
        && (a.caret.0 != b.caret.0 || a.diag != b.diag)
    {
        parts.push(caret_words(b));
    }
    if prev.recording != cur.recording {
        parts.push(if cur.recording { "Recording macro".to_string() } else { "Macro recording stopped".to_string() });
    }
    parts.join(". ")
}

/// "error: message" for the status of a diagnostic (`severity` as in LSP: 1 error .. 4 hint);
/// only the first line of the message, at most 200 chars.
pub fn diag_words(severity: u8, message: &str) -> String {
    let kind = match severity {
        1 => "error",
        2 => "warning",
        3 => "info",
        _ => "hint",
    };
    let first = message.lines().next().unwrap_or("").trim();
    let text: String = first.chars().take(200).collect();
    format!("{kind}: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(u: &TreeUpdate, id: NodeId) -> &Node {
        &u.nodes.iter().find(|(i, _)| *i == id).unwrap_or_else(|| panic!("no node {id:?}")).1
    }

    fn has(u: &TreeUpdate, id: NodeId) -> bool {
        u.nodes.iter().any(|(i, _)| *i == id)
    }

    fn editor(lines: &[&str], first: usize, total: usize, caret: (usize, usize)) -> EditorView {
        EditorView {
            name: "main.rs".into(),
            line_count: total,
            first,
            lines: lines.iter().map(|s| s.to_string()).collect(),
            caret,
            anchor: caret,
            ..Default::default()
        }
    }

    fn snap(e: EditorView) -> Snapshot {
        Snapshot {
            title: "proj".into(),
            tabs: vec![TabView { name: "main.rs".into(), dirty: true, active: true }],
            editor: Some(e),
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_screen_is_a_window_with_two_live_regions() {
        let u = build(&Snapshot::default(), "");
        assert_eq!(node(&u, ROOT).role(), Role::Window);
        assert_eq!(node(&u, ROOT).children(), &[TOAST, ANNOUNCER]);
        assert_eq!(node(&u, TOAST).live(), Some(Live::Polite));
        assert_eq!(node(&u, ANNOUNCER).live(), Some(Live::Polite));
        assert_eq!(u.focus, ROOT);
        assert_eq!(u.tree.as_ref().map(|t| t.root), Some(ROOT));
    }

    #[test]
    fn every_child_exists_and_the_focus_is_in_the_tree() {
        let mut s = snap(editor(&["fn main() {", "}"], 0, 2, (0, 3)));
        s.files = vec![FileRow { name: "src".into(), depth: 0, is_dir: true, expanded: true, selected: true }];
        s.files_sel = Some(0);
        s.find = Some(FindView { query: "x".into(), replace: Some("y".into()), matches: 3, current: Some(1) });
        s.dialog = Some(DialogView {
            title: "Command palette".into(),
            input: Some("op".into()),
            items: vec![DialogItem { label: "Open".into(), detail: "Ctrl+O".into() }],
            sel: Some(0),
        });
        s.terminal =
            Some(TermView { title: "Terminal 1".into(), rows: vec!["$ ls".into(), "a b".into()], cursor: (1, 3) });
        for area in [Area::Editor, Area::Files, Area::Terminal, Area::Find, Area::Palette, Area::Dialog] {
            s.area = area;
            let u = build(&s, "hi");
            for (_, n) in &u.nodes {
                for c in n.children() {
                    assert!(has(&u, *c), "{area:?}: missing child {c:?}");
                }
            }
            assert!(has(&u, u.focus), "{area:?}: focus {:?} not in the tree", u.focus);
            let mut ids: Vec<u64> = u.nodes.iter().map(|(i, _)| i.0).collect();
            let n = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), n, "{area:?}: duplicate node ids");
        }
    }

    #[test]
    fn the_editor_exposes_a_run_per_line_with_the_caret() {
        let s = snap(editor(&["let a = 1;", "héllo wörld"], 5, 40, (6, 4)));
        let u = build(&s, "");
        let ed = node(&u, EDITOR);
        assert_eq!(ed.role(), Role::MultilineTextInput);
        assert_eq!(ed.label(), Some("main.rs"));
        assert_eq!(ed.children(), &[NodeId(LINE_BASE + 5), NodeId(LINE_BASE + 6)]);
        let r0 = node(&u, NodeId(LINE_BASE + 5));
        assert_eq!((r0.role(), r0.value()), (Role::TextRun, Some("let a = 1;\n")));
        assert_eq!(r0.character_lengths().len(), 11);
        assert_eq!(r0.word_starts(), &[0, 4, 6, 8]);
        // Multi-byte chars: one length entry per char, in bytes.
        let r1 = node(&u, NodeId(LINE_BASE + 6));
        assert_eq!(r1.character_lengths().iter().map(|b| *b as usize).sum::<usize>(), "héllo wörld\n".len());
        let sel = ed.text_selection().expect("selection");
        assert_eq!((sel.focus.node, sel.focus.character_index), (NodeId(LINE_BASE + 6), 4));
        assert_eq!(sel.anchor, sel.focus, "a caret is a collapsed selection");
        assert_eq!(u.focus, EDITOR);
    }

    #[test]
    fn the_last_line_of_the_file_has_no_line_break() {
        let s = snap(editor(&["a", "b"], 0, 2, (1, 1)));
        let u = build(&s, "");
        assert_eq!(node(&u, NodeId(LINE_BASE)).value(), Some("a\n"));
        assert_eq!(node(&u, NodeId(LINE_BASE + 1)).value(), Some("b"));
    }

    #[test]
    fn selections_beyond_the_window_clamp_to_its_edges() {
        let mut e = editor(&["one", "two", "three"], 10, 100, (11, 2));
        e.anchor = (2, 7); // above the window
        let sel =
            *build(&snap(e.clone()), "").nodes.iter().find(|(i, _)| *i == EDITOR).unwrap().1.text_selection().unwrap();
        assert_eq!((sel.anchor.node, sel.anchor.character_index), (NodeId(LINE_BASE + 10), 0));
        e.anchor = (50, 0); // below it
        let sel = *build(&snap(e), "").nodes.iter().find(|(i, _)| *i == EDITOR).unwrap().1.text_selection().unwrap();
        assert_eq!((sel.anchor.node, sel.anchor.character_index), (NodeId(LINE_BASE + 12), 5));
    }

    #[test]
    fn a_column_past_the_end_of_a_line_clamps_to_its_length() {
        let s = snap(editor(&["ab"], 0, 1, (0, 99)));
        let u = build(&s, "");
        let sel = node(&u, EDITOR).text_selection().unwrap();
        assert_eq!(sel.focus.character_index, 2);
    }

    #[test]
    fn an_empty_window_still_gets_one_empty_line() {
        let s = snap(editor(&[], 0, 1, (0, 0)));
        let u = build(&s, "");
        assert_eq!(node(&u, EDITOR).children().len(), 1);
        assert_eq!(node(&u, NodeId(LINE_BASE)).value(), Some(""));
    }

    #[test]
    fn tabs_files_and_dialog_items_carry_their_state() {
        let mut s = snap(editor(&["x"], 0, 1, (0, 0)));
        s.files = vec![
            FileRow { name: "src".into(), depth: 0, is_dir: true, expanded: false, selected: false },
            FileRow { name: "lib.rs".into(), depth: 1, is_dir: false, expanded: false, selected: true },
        ];
        s.dialog = Some(DialogView {
            title: "Open".into(),
            input: None,
            items: vec![
                DialogItem { label: "a".into(), detail: String::new() },
                DialogItem { label: "b".into(), detail: "dir".into() },
            ],
            sel: Some(1),
        });
        s.area = Area::Palette;
        let u = build(&s, "");
        let tab = node(&u, NodeId(TAB_BASE));
        assert_eq!((tab.role(), tab.label(), tab.is_selected()), (Role::Tab, Some("main.rs, modified"), Some(true)));
        let dir = node(&u, NodeId(FILE_BASE));
        assert_eq!((dir.role(), dir.level(), dir.is_expanded()), (Role::TreeItem, Some(1), Some(false)));
        let file = node(&u, NodeId(FILE_BASE + 1));
        assert_eq!((file.level(), file.is_expanded(), file.is_selected()), (Some(2), None, Some(true)));
        let item = node(&u, NodeId(ITEM_BASE + 1));
        assert_eq!((item.label(), item.description(), item.is_selected()), (Some("b"), Some("dir"), Some(true)));
        assert_eq!(item.position_in_set(), Some(2));
        assert_eq!(u.focus, NodeId(ITEM_BASE + 1), "the palette's focus is its selected item");
        assert!(node(&u, DIALOG).is_modal());
    }

    #[test]
    fn the_toast_and_the_announcer_are_live_text() {
        let mut s = Snapshot::default();
        s.toast = Some("saved".into());
        let u = build(&s, "Line 3 of 9");
        assert_eq!(node(&u, TOAST).label(), Some("saved"));
        assert_eq!(node(&u, ANNOUNCER).label(), Some("Line 3 of 9"));
    }

    #[test]
    fn terminal_rows_and_cursor() {
        let mut s = Snapshot::default();
        s.area = Area::Terminal;
        s.terminal =
            Some(TermView { title: "Terminal 1".into(), rows: vec!["$ ls".into(), "".into()], cursor: (0, 99) });
        let u = build(&s, "");
        let t = node(&u, TERMINAL);
        assert_eq!((t.role(), t.label()), (Role::Terminal, Some("Terminal 1")));
        assert_eq!(t.children().len(), 2);
        assert_eq!(t.text_selection().unwrap().focus.character_index, 4);
        assert_eq!(u.focus, TERMINAL);
    }

    #[test]
    fn the_find_bar_summarizes_its_matches() {
        let mut s = Snapshot::default();
        s.find = Some(FindView { query: "foo".into(), replace: None, matches: 4, current: Some(1) });
        let u = build(&s, "");
        assert_eq!(node(&u, FIND).description(), Some("match 2 of 4"));
        assert_eq!(node(&u, FIND_INPUT).value(), Some("foo"));
        assert!(!has(&u, REPLACE_INPUT));
        s.find = Some(FindView { query: "zz".into(), matches: 0, ..Default::default() });
        assert_eq!(node(&build(&s, ""), FIND).description(), Some("no matches"));
    }

    #[test]
    fn windows_follow_the_viewport_the_caret_and_the_cap() {
        assert_eq!(editor_window(0, 0, 10, 0, 0), (0, 1));
        assert_eq!(editor_window(100, 10, 20, 15, 15), (8, 32));
        // Caret outside the viewport widens the window.
        assert_eq!(editor_window(100, 10, 20, 60, 60), (8, 61));
        assert_eq!(editor_window(100, 10, 20, 0, 0).0, 0);
        // End of file.
        assert_eq!(editor_window(30, 25, 20, 29, 29), (23, 30));
        // A selection of the whole file is capped, the caret stays inside.
        let (a, b) = editor_window(5000, 0, 30, 4000, 0);
        assert!(b - a <= MAX_EDITOR_LINES && a <= 4000 && 4000 < b, "{a}..{b}");
        let (a, b) = editor_window(5000, 0, 30, 0, 4000);
        assert!(b - a <= MAX_EDITOR_LINES && a == 0 && b > 0, "{a}..{b}");
    }

    #[test]
    fn list_windows_keep_the_selection_inside() {
        assert_eq!(list_window(10, 3, 50), (0, 10));
        for sel in [0, 1, 40, 99] {
            let (a, b) = list_window(100, sel, 50);
            assert_eq!(b - a, 50);
            assert!(a <= sel && sel < b && b <= 100, "{sel}: {a}..{b}");
        }
    }

    #[test]
    fn nothing_is_said_on_the_first_snapshot_or_when_nothing_moved() {
        let s = snap(editor(&["a"], 0, 1, (0, 0)));
        assert_eq!(announcement(None, &s), "");
        assert_eq!(announcement(Some(&s), &s), "");
        // Moving inside a line is the screen reader's job.
        let mut t = s.clone();
        t.editor.as_mut().unwrap().caret = (0, 1);
        assert_eq!(announcement(Some(&s), &t), "");
    }

    #[test]
    fn a_new_caret_line_is_announced_with_its_diagnostic() {
        let a = snap(editor(&["a", "b", "c"], 0, 3, (0, 0)));
        let mut b = a.clone();
        b.editor.as_mut().unwrap().caret = (2, 0);
        assert_eq!(announcement(Some(&a), &b), "Line 3 of 3");
        b.editor.as_mut().unwrap().diag = Some(diag_words(1, "expected `;`\nmore"));
        assert_eq!(announcement(Some(&a), &b), "Line 3 of 3. error: expected `;`");
        // The diagnostic appearing on the same line is announced too.
        let c = b.clone();
        let mut d = b.clone();
        d.editor.as_mut().unwrap().diag = Some("warning: unused".into());
        assert_eq!(announcement(Some(&c), &d), "Line 3 of 3. warning: unused");
    }

    #[test]
    fn modes_and_areas_are_announced() {
        let a = snap(editor(&["a"], 0, 1, (0, 0)));
        let mut b = a.clone();
        b.area = Area::Files;
        assert_eq!(announcement(Some(&a), &b), "File explorer");
        let mut c = a.clone();
        c.area = Area::Find;
        assert_eq!(announcement(Some(&a), &c), "Find");
        let mut d = a.clone();
        d.recording = true;
        assert_eq!(announcement(Some(&a), &d), "Recording macro");
        assert_eq!(announcement(Some(&d), &a), "Macro recording stopped");
        let mut p = a.clone();
        p.area = Area::Palette;
        p.dialog = Some(DialogView { title: "Go to file".into(), ..Default::default() });
        assert_eq!(announcement(Some(&a), &p), "Go to file");
        let mut t = a.clone();
        t.editor.as_mut().unwrap().name = "other.rs".into();
        assert_eq!(announcement(Some(&a), &t), "Editor, other.rs");
    }

    #[test]
    fn diagnostic_wording() {
        assert_eq!(diag_words(2, "  unused variable  "), "warning: unused variable");
        assert_eq!(diag_words(4, ""), "hint: ");
        assert_eq!(diag_words(1, &"x".repeat(500)).chars().count(), 7 + 200);
    }
}
