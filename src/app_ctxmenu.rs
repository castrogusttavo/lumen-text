//! Context menus (right click): sidebar folders and files, editor tabs.
//!
//! The menu is an [`Overlay::Ctx`]: a floating rounded card with a soft shadow, thin separators,
//! a HugeIcons/Zed icon and the shortcut (when there is one) on the right of every item. The
//! hover (and the keyboard selection, same look) is a low-opacity tint of the foreground, so the
//! text keeps its colour and stays readable on any theme. Arrows + Enter, Esc or a click outside
//! close/accept it. "Delete" asks for confirmation with a small Yes/No card (same overlay).
//!
//! File operations (cut/copy/paste/duplicate/trash) are plain functions at the top of the file
//! (unit tested); the `impl App` part wires them to the tree, open tabs and the watcher.
//! Pinned tabs are a flag on [`Doc`]: they are drawn first (display order only, so document
//! indices never move), show a pin instead of the close button and survive "Close All".

use crate::{
    app::{App, Focus, ROW_H, TAB_H},
    app_overlay::{Overlay, Prompt, PromptKind},
    doc::Doc,
    field::TextField,
    ui::{Canvas, Rect},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use winit::{
    event::MouseButton,
    keyboard::{Key, NamedKey},
};

const ITEM_H: i32 = 28;
const SEP_H: i32 = 9;
const PAD: i32 = 6;

// ───────────────────────── pure file helpers ─────────────────────────

/// Paths copied/cut inside the app (the "file clipboard").
#[derive(Clone, Debug, Default)]
pub struct FileClip {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

impl FileClip {
    /// Is `p` waiting to be moved (cut, or inside a cut folder)? The explorer draws it dimmed.
    pub fn is_cut(&self, p: &Path) -> bool {
        self.cut && self.paths.iter().any(|c| p.starts_with(c))
    }
}

/// `(stem, extension-with-dot)`; dotfiles have no extension, `.tar.gz` is one extension.
pub fn split_name(name: &str) -> (&str, &str) {
    for ext in [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst"] {
        if name.len() > ext.len() && name.ends_with(ext) {
            return (&name[..name.len() - ext.len()], &name[name.len() - ext.len()..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// "foo (copy 3)" -> "foo".
fn strip_copy_suffix(stem: &str) -> &str {
    // Also accepts the old Portuguese "(cópia)" spelling of files made by earlier versions.
    let found = [" (copy", " (cópia"].iter().filter_map(|m| stem.rfind(m).map(|i| (i, m.len()))).max();
    if let Some((open, mlen)) = found {
        let tail = &stem[open + mlen..];
        let ok = tail == ")"
            || (tail.starts_with(' ')
                && tail.ends_with(')')
                && tail[1..tail.len() - 1].chars().all(|c| c.is_ascii_digit())
                && tail.len() > 2);
        if ok {
            return &stem[..open];
        }
    }
    stem
}

fn taken(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// `dir/name`, or `dir/stem (copy).ext`, `dir/stem (copy 2).ext`… when something is already there.
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    unique_dest_by(dir, name, taken)
}

pub fn unique_dest_by(dir: &Path, name: &str, is_taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !is_taken(&first) {
        return first;
    }
    let (stem, ext) = split_name(name);
    let stem = strip_copy_suffix(stem);
    for n in 1u32.. {
        let cand = if n == 1 { format!("{stem} (copy){ext}") } else { format!("{stem} (copy {n}){ext}") };
        let p = dir.join(cand);
        if !is_taken(&p) {
            return p;
        }
    }
    unreachable!()
}

/// Copy a file, a symlink or a whole directory tree to `to` (which must not exist).
pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        crate::paths::symlink(&fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        let _ = fs::set_permissions(to, meta.permissions());
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

fn remove_any(p: &Path) -> io::Result<()> {
    if fs::symlink_metadata(p)?.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
}

/// Rename, falling back to copy + remove across filesystems.
pub fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_recursive(from, to)?;
            remove_any(from)
        }
        r => r,
    }
}

/// Paste (or move, when `cut`) one item into `dir`. `Ok(None)` = nothing to do (cut into the
/// folder it already lives in); `Ok(Some(to))` = the new path.
pub fn paste_one(from: &Path, dir: &Path, cut: bool) -> Result<Option<PathBuf>, String> {
    if !taken(from) {
        return Err(format!(
            "{} no longer exists",
            from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
    }
    if from.is_dir() && dir.starts_with(from) {
        return Err("cannot paste a folder into itself".into());
    }
    if cut && from.parent() == Some(dir) {
        return Ok(None);
    }
    let name = from.file_name().ok_or("invalid path")?.to_string_lossy().into_owned();
    let to = unique_dest(dir, &name);
    let res = if cut { move_path(from, &to) } else { copy_recursive(from, &to) };
    res.map(|()| Some(to)).map_err(|e| e.to_string())
}

fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn local_stamp() -> String {
    let (y, mo, d, h, mi, s) = crate::paths::local_time(crate::paths::now_unix());
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// Freedesktop trash fallback: move `path` into `<trash>/files` and write `<trash>/info/*.trashinfo`.
pub fn trash_into(path: &Path, trash: &Path) -> io::Result<PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let name = abs.file_name().ok_or_else(|| io::Error::other("invalid path"))?.to_string_lossy().into_owned();
    let info_of = |p: &Path| {
        info.join(format!("{}.trashinfo", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
    };
    let dest = unique_dest_by(&files, &name, |p| taken(p) || taken(&info_of(p)));
    let info_file = info_of(&dest);
    fs::write(
        &info_file,
        format!("[Trash Info]\nPath={}\nDeletionDate={}\n", pct_encode(&abs.to_string_lossy()), local_stamp()),
    )?;
    if let Err(e) = move_path(&abs, &dest) {
        let _ = fs::remove_file(&info_file);
        return Err(e);
    }
    Ok(dest)
}

fn user_trash() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))?;
    Some(base.join("Trash"))
}

/// Move to the Recycle Bin (`SHFileOperationW` with undo).
#[cfg(windows)]
pub fn trash_path(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW,
    };
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(path)
    };
    // The list is double-NUL terminated.
    let wide: Vec<u16> = abs.as_os_str().encode_wide().chain([0, 0]).collect();
    // SAFETY: `wide` outlives the call and the struct is plain data.
    let rc = unsafe {
        let mut op: SHFILEOPSTRUCTW = std::mem::zeroed();
        op.wFunc = FO_DELETE;
        op.pFrom = wide.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as _;
        SHFileOperationW(&mut op)
    };
    if rc == 0 { Ok(()) } else { Err(format!("could not move to the Recycle Bin (error {rc})")) }
}

/// Move to the Trash through Finder.
#[cfg(target_os = "macos")]
pub fn trash_path(path: &Path) -> Result<(), String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(path)
    };
    let quoted = abs.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("tell application \"Finder\" to delete POSIX file \"{quoted}\"");
    let ok = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("could not move to the Trash".into()) }
}

/// Move to the trash: `gio trash`, else the freedesktop trash by hand.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn trash_path(path: &Path) -> Result<(), String> {
    let ok = Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok && !taken(path) {
        return Ok(());
    }
    let trash = user_trash().ok_or("no trash folder")?;
    trash_into(path, &trash).map(|_| ()).map_err(|e| e.to_string())
}

// ───────────────────────── pinned tabs ─────────────────────────

/// Display order of the tabs: pinned first, otherwise document order.
pub fn pinned_first(pinned: &[bool]) -> Vec<usize> {
    let mut v: Vec<usize> = (0..pinned.len()).collect();
    v.sort_by_key(|&i| !pinned[i]);
    v
}

pub fn tab_order(docs: &[Doc]) -> Vec<usize> {
    let p: Vec<bool> = docs.iter().map(|d| d.pinned).collect();
    pinned_first(&p)
}

// ───────────────────────── menu model ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Folder(PathBuf),
    File(PathBuf),
    /// Empty area of the explorer: the project root.
    Root(PathBuf),
    /// Several rows of the explorer (the menu acts on all of them).
    Multi(Vec<PathBuf>),
    /// Document index.
    Tab(usize),
    /// A commit row of the Git panel's History tab.
    Commit(crate::app_gitpanel::CommitRef),
    /// Changed files of the Git panel (the selection, or the file right-clicked).
    GitFiles(Vec<PathBuf>),
    /// The text of the active document.
    Editor,
    /// The explorer's header (Files / Outline tabs): the "Sort by" menu, with the current order
    /// and the folders-first switch.
    ExplorerHeader(crate::tree::SortMode, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Duplicate,
    Rename,
    Trash,
    Delete,
    CopyPath,
    OpenSide,
    CloseTab,
    CloseOthers,
    CloseAll,
    TogglePin,
    Markdown,
    /// Snapshots of the file kept by the local history (see `app_localhist`).
    LocalHistory,
    /// Move the tab to a new window (see `app_window`).
    NewWindow,
    // Commit rows (see `app_gitpanel::actions`).
    CherryPick,
    CopyHash,
    BranchHere,
    TagHere,
    UndoCommit,
    Checkout,
    Revert,
    /// Changed files of the Git panel: stash them.
    StashFiles,
    /// Run a palette command by id (the editor's menu).
    Run(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Sep,
    Item { act: Act, label: &'static str, icon: &'static str, key: &'static str, danger: bool },
}

/// What the menu depends on besides the target.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub has_clip: bool,
    pub split: bool,
    /// Tab is a Markdown file: `Some(reader_open)`.
    pub md: Option<bool>,
    pub pinned: bool,
    pub has_path: bool,
    /// A closed tab can be reopened (tab menu: "Reopen Closed Tab").
    pub reopen: bool,
}

fn item(act: Act, label: &'static str, icon: &'static str, key: &'static str) -> Entry {
    Entry::Item { act, label, icon, key, danger: false }
}

/// Drop leading/trailing/duplicate separators.
fn tidy(v: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for e in v {
        if e == Entry::Sep && matches!(out.last(), None | Some(Entry::Sep)) {
            continue;
        }
        out.push(e);
    }
    while out.last() == Some(&Entry::Sep) {
        out.pop();
    }
    out
}

pub fn entries_for(t: &Target, f: &Flags) -> Vec<Entry> {
    let sep = || Entry::Sep;
    let trash = || item(Act::Trash, "Move to Trash", "hugeicons/trash", "");
    let delete = || Entry::Item { act: Act::Delete, label: "Delete", icon: "ui/delete", key: "Del", danger: true };
    let v = match t {
        Target::Folder(_) | Target::Root(_) => {
            let mut v = vec![
                item(Act::NewFile, "New File", "hugeicons/plus", "A"),
                item(Act::NewFolder, "New Folder", "ui/folder_add", "Shift+A"),
                sep(),
            ];
            if matches!(t, Target::Folder(_)) {
                v.extend([item(Act::Cut, "Cut", "ui/scissors", ""), item(Act::Copy, "Copy", "hugeicons/copy", "")]);
            }
            if f.has_clip {
                v.push(item(Act::Paste, "Paste", "hugeicons/download", ""));
            }
            if matches!(t, Target::Folder(_)) {
                v.extend([
                    sep(),
                    item(Act::Duplicate, "Duplicate", "ui/file_multiple", ""),
                    item(Act::Rename, "Rename", "hugeicons/edit", "F2"),
                    sep(),
                    trash(),
                    delete(),
                ]);
            }
            v
        }
        Target::File(_) => {
            let mut v = vec![
                item(Act::Cut, "Cut", "ui/scissors", ""),
                item(Act::Copy, "Copy", "hugeicons/copy", ""),
                sep(),
                item(Act::Duplicate, "Duplicate", "ui/file_multiple", ""),
                item(Act::Rename, "Rename", "hugeicons/edit", "F2"),
                sep(),
                trash(),
                delete(),
                sep(),
                item(Act::CopyPath, "Copy Path", "hugeicons/link", ""),
                item(Act::LocalHistory, "Local History", "ui/clock", ""),
            ];
            if f.split {
                v.push(item(Act::OpenSide, "Open to the Side", "ui/split", ""));
            }
            v
        }
        Target::Multi(_) => vec![
            item(Act::Cut, "Cut", "ui/scissors", "Ctrl+X"),
            item(Act::Copy, "Copy", "hugeicons/copy", "Ctrl+C"),
            sep(),
            item(Act::Duplicate, "Duplicate", "ui/file_multiple", ""),
            sep(),
            trash(),
            delete(),
            sep(),
            item(Act::CopyPath, "Copy Paths", "hugeicons/link", ""),
        ],
        Target::Tab(_) => {
            let mut v = vec![
                item(Act::CloseTab, "Close", "ui/close", "Ctrl+W"),
                item(Act::CloseOthers, "Close Others", "ui/x_circle", ""),
                item(Act::CloseAll, "Close All", "ui/list_x", ""),
            ];
            if f.reopen {
                v.push(item(Act::Run("ReopenClosedTab"), "Reopen Closed Tab", "ui/undo", "Ctrl+Shift+T"));
            }
            v.extend([
                sep(),
                if f.pinned {
                    item(Act::TogglePin, "Unpin Tab", "ui/unpin", "")
                } else {
                    item(Act::TogglePin, "Pin Tab", "ui/pin", "")
                },
                item(Act::NewWindow, "Move to New Window", "ui/arrow_up_right", ""),
                sep(),
            ]);
            if f.has_path {
                v.push(item(Act::CopyPath, "Copy Path", "hugeicons/link", ""));
                v.push(item(Act::LocalHistory, "Local History", "ui/clock", ""));
            }
            if let Some(open) = f.md {
                v.push(item(
                    Act::Markdown,
                    if open { "Close Markdown Preview" } else { "Open Markdown Preview" },
                    "ui/file_markdown",
                    "Ctrl+Shift+V",
                ));
            }
            v
        }
        Target::Editor => vec![
            item(Act::Run("AiSendSelection"), "Send Selection to Agent", "ui/at_sign", "Ctrl+Alt+K"),
            sep(),
            item(Act::Run("GotoDefinition"), "Go to Definition", "ui/arrow_up_right", "F12"),
            item(Act::Run("FindReferences"), "Find References", "ui/magnifying_glass", "Shift+F12"),
            item(Act::Run("Rename"), "Rename Symbol", "ui/pencil", "F2"),
            sep(),
            item(Act::Run("Format"), "Format Document", "ui/text_snippet", "Shift+Alt+F"),
        ],
        Target::GitFiles(v) => vec![
            item(Act::StashFiles, if v.len() == 1 { "Stash File…" } else { "Stash Selected Files…" }, "ui/archive", ""),
            sep(),
            item(Act::CopyPath, "Copy Path", "hugeicons/link", ""),
        ],
        Target::ExplorerHeader(mode, first) => {
            use crate::tree::SortMode;
            let mark = |on: bool| if on { "ui/check" } else { "" };
            vec![
                item(Act::Run("ExplorerSortName"), "Sort by Name", mark(*mode == SortMode::Name), ""),
                item(Act::Run("ExplorerSortType"), "Sort by Type", mark(*mode == SortMode::Type), ""),
                item(Act::Run("ExplorerSortModified"), "Sort by Date Modified", mark(*mode == SortMode::Modified), ""),
                item(Act::Run("ExplorerSortMixed"), "Mixed: Folders Among Files", mark(*mode == SortMode::Mixed), ""),
                sep(),
                item(Act::Run("ToggleFoldersFirst"), "Folders First", mark(*first && *mode != SortMode::Mixed), ""),
            ]
        }
        Target::Commit(c) => {
            let mut v = vec![
                item(Act::Checkout, "Checkout", "ui/git_branch", ""),
                sep(),
                item(Act::BranchHere, "Create Branch Here…", "ui/git_branch_plus", ""),
                item(Act::TagHere, "Create Tag Here…", "ui/hash", ""),
                sep(),
                item(Act::CherryPick, "Cherry-pick", "ui/git_commit", ""),
                item(Act::Revert, "Revert", "ui/rotate_ccw", ""),
                sep(),
                item(Act::CopyHash, "Copy Hash", "hugeicons/copy", ""),
            ];
            if c.head {
                v.extend([sep(), item(Act::UndoCommit, "Undo Last Commit", "ui/undo", "")]);
            }
            v
        }
    };
    tidy(v)
}

/// Next selectable entry from `sel` in direction `dir` (+1/-1), wrapping; separators are skipped.
pub fn step(entries: &[Entry], sel: Option<usize>, dir: i32) -> Option<usize> {
    let n = entries.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match (sel, dir > 0) {
        (Some(s), _) => s as i32,
        (None, true) => -1,
        (None, false) => n,
    };
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if matches!(entries[i as usize], Entry::Item { .. }) {
            return Some(i as usize);
        }
    }
    None
}

pub struct Confirm {
    /// What "Yes" deletes (one or several explorer items).
    pub paths: Vec<PathBuf>,
    /// Focused button: true = "Yes". Starts on "No".
    pub yes: bool,
    /// Buttons ("No", "Yes") as drawn last frame.
    pub rects: [Rect; 2],
}

pub struct CtxMenu {
    pub target: Target,
    pub entries: Vec<Entry>,
    /// Where the menu was opened (window coordinates).
    pub at: (i32, i32),
    /// Highlighted entry (hover or keyboard).
    pub sel: Option<usize>,
    /// Item rects drawn last frame.
    pub rects: Vec<(Rect, usize)>,
    /// The card drawn last frame.
    pub card: Rect,
    pub confirm: Option<Confirm>,
    /// "Save all / Discard / Cancel" card (see `app_unsaved.rs`).
    pub unsaved: Option<crate::app_unsaved::Unsaved>,
    /// Two-button question: running processes, "Save as administrator" (see `app_ask.rs`).
    pub ask: Option<crate::app_ask::Ask>,
}

// ───────────────────────── App integration ─────────────────────────

impl App {
    /// Right button: open the menu for the tab or tree row under the pointer.
    pub fn right_click(&mut self) {
        let (x, y) = self.mouse;
        if matches!(self.overlay, Overlay::Ctx(_)) {
            self.overlay = Overlay::None;
        } else if !self.overlay.is_none() {
            return;
        }
        if self.gitpanel_right_click(x, y) || self.md_copy_tex(x, y) {
            return;
        }
        self.hover = None;
        self.completion = None;
        if let Some(t) = self.ctx_target_at(x, y) {
            self.open_ctxmenu(t, (x as i32, y as i32));
        }
        self.request_redraw();
    }

    fn ctx_target_at(&mut self, x: f64, y: f64) -> Option<Target> {
        if self.layout.tabbar.contains(x, y) {
            return self.layout.tabs.iter().find(|(r, _, _)| r.contains(x, y)).map(|&(_, _, i)| Target::Tab(i));
        }
        let sb = self.layout.sidebar;
        if self.sidebar_open && sb.contains(x, y) && self.sidebar_mode == crate::app::SidebarMode::Files {
            if y < (sb.y + TAB_H) as f64 {
                return Some(Target::ExplorerHeader(self.tree.sort(), self.tree.folders_first()));
            }
            let rel = y - (sb.y + TAB_H) as f64 + self.tree.scroll;
            let row = (rel >= 0.0).then(|| (rel / ROW_H as f64) as usize).filter(|&i| i < self.tree.rows.len());
            return Some(match row {
                Some(i) => {
                    self.focus = Focus::Sidebar;
                    // Right click on a row of a multi-selection: the menu acts on all of it.
                    let sel = self.tree.selection();
                    if sel.len() > 1 && self.tree.is_selected(&self.tree.rows[i].path) {
                        return Some(Target::Multi(sel));
                    }
                    self.tree.select_only(i);
                    let r = &self.tree.rows[i];
                    if r.is_dir { Target::Folder(r.path.clone()) } else { Target::File(r.path.clone()) }
                }
                None => Target::Root(self.root.clone()),
            });
        }
        // The editor's text (not under an open diff): the caret moves to the click unless it lands
        // inside the selection, like other editors.
        let ed = self.layout.editor;
        if ed.contains(x, y) && !self.docs.is_empty() && !(self.diff.is_some() && self.diff_area().contains(x, y)) {
            self.focus = Focus::Editor;
            let d = &mut self.docs[self.active];
            let p = d.pos_at(x, y);
            if d.ed.sel.is_empty() || p < d.ed.sel.start() || p > d.ed.sel.end() {
                d.ed.click(&mut d.buf, p, false);
            }
            return Some(Target::Editor);
        }
        None
    }

    pub fn open_ctxmenu(&mut self, target: Target, at: (i32, i32)) {
        let mut f = Flags {
            has_clip: !self.file_clip.paths.is_empty() || self.clip.get_files().is_some(),
            split: self.split.is_some(),
            ..Flags::default()
        };
        if let Target::Tab(i) = &target {
            let Some(d) = self.docs.get(*i) else { return };
            f.pinned = d.pinned;
            f.has_path = d.path().is_some();
            f.reopen = !self.closed_tabs.is_empty();
            f.md = crate::app_md::is_markdown_name(&d.name).then(|| d.md.is_some());
        }
        let entries = entries_for(&target, &f);
        self.overlay = Overlay::Ctx(Box::new(CtxMenu {
            target,
            entries,
            at,
            sel: None,
            rects: Vec::new(),
            card: Rect::default(),
            confirm: None,
            unsaved: None,
            ask: None,
        }));
    }

    pub fn ctxmenu_key(&mut self, ev: &crate::app::KeyIn) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        if m.unsaved.is_some() {
            return self.unsaved_key(&ev.key);
        }
        if m.ask.is_some() {
            return self.ask_key(&ev.key);
        }
        if let Some(c) = m.confirm.as_mut() {
            match &ev.key {
                Key::Named(NamedKey::Escape) => self.overlay = Overlay::None,
                Key::Named(NamedKey::ArrowLeft) => c.yes = false,
                Key::Named(NamedKey::ArrowRight) => c.yes = true,
                Key::Named(NamedKey::Tab) => c.yes = !c.yes,
                Key::Named(NamedKey::Enter) => {
                    let (yes, paths) = (c.yes, c.paths.clone());
                    self.overlay = Overlay::None;
                    if yes {
                        self.delete_confirmed(&paths);
                    } else {
                        self.say("delete cancelled");
                    }
                }
                Key::Character(s) if matches!(s.to_lowercase().as_str(), "s" | "y") => {
                    let paths = c.paths.clone();
                    self.overlay = Overlay::None;
                    self.delete_confirmed(&paths);
                }
                Key::Character(s) if s.to_lowercase() == "n" => self.overlay = Overlay::None,
                _ => {}
            }
            self.request_redraw();
            return;
        }
        match &ev.key {
            Key::Named(NamedKey::Escape) => self.overlay = Overlay::None,
            Key::Named(NamedKey::ArrowDown) => m.sel = step(&m.entries, m.sel, 1),
            Key::Named(NamedKey::ArrowUp) => m.sel = step(&m.entries, m.sel, -1),
            Key::Named(NamedKey::Tab) => m.sel = step(&m.entries, m.sel, if self.mods.shift_key() { -1 } else { 1 }),
            Key::Named(NamedKey::Home) => m.sel = step(&m.entries, None, 1),
            Key::Named(NamedKey::End) => m.sel = step(&m.entries, None, -1),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(i) = m.sel {
                    self.ctxmenu_pick(i);
                }
            }
            _ => {}
        }
        self.request_redraw();
    }

    /// Left click while the menu is open. Always consumes the click.
    pub fn ctxmenu_mouse(&mut self, x: f64, y: f64) -> bool {
        let Overlay::Ctx(m) = &mut self.overlay else { return false };
        if m.unsaved.is_some() {
            self.unsaved_mouse(x, y);
            return true;
        }
        if m.ask.is_some() {
            self.ask_mouse(x, y);
            return true;
        }
        if let Some(c) = m.confirm.as_ref() {
            let (no, yes, paths) = (c.rects[0], c.rects[1], c.paths.clone());
            if yes.contains(x, y) {
                self.overlay = Overlay::None;
                self.delete_confirmed(&paths);
            } else if no.contains(x, y) || !m.card.contains(x, y) {
                self.overlay = Overlay::None;
                self.say("delete cancelled");
            }
            return true;
        }
        if let Some(&(_, i)) = m.rects.iter().find(|(r, _)| r.contains(x, y)) {
            self.ctxmenu_pick(i);
        } else if !m.card.contains(x, y) {
            self.overlay = Overlay::None;
        }
        true
    }

    /// Pointer moved: the hovered item (or confirm button) takes the highlight.
    pub fn ctxmenu_hover(&mut self) -> bool {
        self.update_tooltip();
        let (x, y) = self.mouse;
        let Overlay::Ctx(m) = &mut self.overlay else { return false };
        if m.unsaved.is_some() {
            return self.unsaved_hover(x, y);
        }
        if m.ask.is_some() {
            return self.ask_hover(x, y);
        }
        if let Some(c) = m.confirm.as_mut() {
            if c.rects[1].contains(x, y) && !c.yes {
                c.yes = true;
                return true;
            }
            if c.rects[0].contains(x, y) && c.yes {
                c.yes = false;
                return true;
            }
            return false;
        }
        match m.rects.iter().find(|(r, _)| r.contains(x, y)) {
            Some(&(_, i)) if m.sel != Some(i) => {
                m.sel = Some(i);
                true
            }
            _ => false,
        }
    }

    /// Esc in the explorer while something is cut: forget the cut (rows stop being dimmed).
    /// True when there was a cut to cancel.
    pub fn cancel_cut(&mut self) -> bool {
        if !self.file_clip.cut || self.file_clip.paths.is_empty() {
            return false;
        }
        self.file_clip = FileClip::default();
        self.say("cut cancelled");
        self.request_redraw();
        true
    }

    pub fn ctxmenu_active(&self) -> bool {
        matches!(self.overlay, Overlay::Ctx(_))
    }

    fn ctxmenu_pick(&mut self, i: usize) {
        let Overlay::Ctx(m) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        if let Some(Entry::Item { act, .. }) = m.entries.get(i) {
            self.ctx_run(*act, m.target);
        }
        self.request_redraw();
    }

    // ── actions ──

    fn ctx_path(&self, t: &Target) -> Option<PathBuf> {
        match t {
            Target::Folder(p) | Target::File(p) | Target::Root(p) => Some(p.clone()),
            Target::Multi(v) => v.first().cloned(),
            Target::Tab(i) => self.docs.get(*i).and_then(|d| d.path()).map(Path::to_path_buf),
            Target::Commit(_) | Target::ExplorerHeader(..) => None,
            Target::GitFiles(v) => v.first().cloned(),
            Target::Editor => self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf),
        }
    }

    /// Delete confirmed in the card: one path, or the whole selection.
    fn delete_confirmed(&mut self, paths: &[PathBuf]) {
        match paths {
            [one] => self.delete_path(one),
            many => self.remove_paths(many, false),
        }
    }

    /// Delete key with several explorer rows selected: the confirmation card for all of them.
    pub fn ctx_delete_selection(&mut self) {
        let paths = self.tree.selection();
        self.ctx_run_multi(Act::Delete, paths);
    }

    /// Menu actions on a multi-selection.
    fn ctx_run_multi(&mut self, act: Act, paths: Vec<PathBuf>) {
        match act {
            Act::Cut | Act::Copy => {
                let cut = act == Act::Cut;
                self.say(format!("{}: {}", if cut { "cut" } else { "copied" }, crate::app_explorer::describe(&paths)));
                self.set_file_clip(paths, cut);
            }
            Act::Duplicate => {
                let mut made = Vec::new();
                for p in &paths {
                    let Some(dir) = p.parent() else { continue };
                    if let Ok(Some(to)) = paste_one(p, dir, false) {
                        made.push(to);
                    }
                }
                if let Some(first) = made.first().cloned() {
                    self.after_fs_change(&first);
                    self.tree.select_paths(&made);
                }
                self.say(format!("{} item(s) duplicated", made.len()));
            }
            Act::Trash => self.remove_paths(&paths, true),
            Act::Delete => {
                self.overlay = Overlay::Ctx(Box::new(CtxMenu {
                    target: Target::Multi(paths.clone()),
                    entries: Vec::new(),
                    at: (0, 0),
                    sel: None,
                    rects: Vec::new(),
                    card: Rect::default(),
                    confirm: Some(Confirm { paths, yes: false, rects: [Rect::default(); 2] }),
                    unsaved: None,
                    ask: None,
                }));
            }
            Act::CopyPath => {
                let text = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
                self.clip.set(&text);
                self.say("paths copied");
            }
            _ => {}
        }
    }

    fn ctx_run(&mut self, act: Act, t: Target) {
        if let Target::Multi(paths) = &t {
            return self.ctx_run_multi(act, paths.clone());
        }
        if let Target::Commit(c) = t {
            return self.git_commit_menu_act(act, c);
        }
        if let (Target::GitFiles(paths), Act::StashFiles) = (&t, act) {
            return self.git_stash_files(paths);
        }
        if let Act::Run(id) = act {
            self.run_bound(id);
            return;
        }
        let path = self.ctx_path(&t);
        let name =
            path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match act {
            Act::NewFile | Act::NewFolder => {
                let Some(dir) = path else { return };
                let is_dir = act == Act::NewFolder;
                let rel = self.rel(&dir);
                let title = format!(
                    "{} in {}",
                    if is_dir { "New Folder" } else { "New File" },
                    if rel.is_empty() { "." } else { &rel }
                );
                let kind = if is_dir { PromptKind::NewDir { dir } } else { PromptKind::NewFile { dir } };
                self.overlay = Overlay::Prompt(Prompt::new(kind, title, TextField::default()));
            }
            Act::Cut | Act::Copy => {
                let Some(p) = path else { return };
                let cut = act == Act::Cut;
                self.set_file_clip(vec![p], cut);
                self.say(format!("{}: {name}", if cut { "cut" } else { "copied" }));
            }
            Act::Paste => {
                if let Some(dir) = path {
                    self.paste_files(&dir);
                }
            }
            Act::Duplicate => {
                let Some(p) = path else { return };
                let Some(dir) = p.parent().map(Path::to_path_buf) else { return };
                match paste_one(&p, &dir, false) {
                    Ok(Some(to)) => {
                        self.after_fs_change(&to);
                        self.say(format!(
                            "duplicated: {}",
                            to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => self.say(format!("error duplicating: {e}")),
                }
            }
            Act::Rename => {
                let Some(p) = path else { return };
                self.overlay = Overlay::Prompt(Prompt::new(
                    PromptKind::RenamePath { from: p },
                    format!("Rename {name}"),
                    TextField::with(&name),
                ));
            }
            Act::Trash => {
                let Some(p) = path else { return };
                match trash_path(&p) {
                    Ok(()) => {
                        self.close_docs_under(&p);
                        self.refresh_tree();
                        self.watch_dirs();
                        self.say(format!("moved to trash: {name}"));
                    }
                    Err(e) => self.say(format!("error moving to trash: {e}")),
                }
            }
            Act::Delete => {
                let Some(p) = path else { return };
                self.overlay = Overlay::Ctx(Box::new(CtxMenu {
                    target: t,
                    entries: Vec::new(),
                    at: (0, 0),
                    sel: None,
                    rects: Vec::new(),
                    card: Rect::default(),
                    confirm: Some(Confirm { paths: vec![p], yes: false, rects: [Rect::default(); 2] }),
                    unsaved: None,
                    ask: None,
                }));
            }
            Act::CopyPath => {
                if let Some(p) = path {
                    self.clip.set(&p.to_string_lossy());
                    self.say("path copied");
                }
            }
            Act::OpenSide => {
                if let Some(p) = path {
                    self.open_beside(&p);
                }
            }
            Act::CloseTab => {
                if let Target::Tab(i) = t {
                    self.close_tab(i, false);
                }
            }
            Act::CloseOthers | Act::CloseAll => {
                let keep = match (&t, act) {
                    (Target::Tab(i), Act::CloseOthers) => Some(*i),
                    _ => None,
                };
                // Unsaved tabs among them: asks (Save all / Discard / Cancel) first.
                self.close_tabs_request(keep);
            }
            Act::TogglePin => {
                if let Target::Tab(i) = t
                    && let Some(d) = self.docs.get_mut(i)
                {
                    d.pinned = !d.pinned;
                    let msg =
                        if d.pinned { format!("tab pinned: {}", d.name) } else { format!("tab unpinned: {}", d.name) };
                    self.say(msg);
                }
            }
            Act::Markdown => {
                if let Target::Tab(i) = t {
                    self.activate(i);
                    self.toggle_md_reader();
                }
            }
            Act::LocalHistory => {
                if let Some(p) = path {
                    self.hist_list(&p);
                }
            }
            Act::NewWindow => {
                if let Target::Tab(i) = t {
                    self.move_tab_to_new_window(i, None);
                }
            }
            Act::CherryPick
            | Act::CopyHash
            | Act::BranchHere
            | Act::TagHere
            | Act::UndoCommit
            | Act::Checkout
            | Act::Revert
            | Act::StashFiles
            | Act::Run(_) => {}
        }
    }

    fn after_fs_change(&mut self, reveal: &Path) {
        self.refresh_tree();
        self.tree.reveal(reveal);
        self.watch_dirs();
    }

    pub fn close_docs_under(&mut self, path: &Path) {
        let mut i = 0;
        while i < self.docs.len() {
            if self.docs[i].path().is_some_and(|p| p.starts_with(path)) {
                self.close_tab(i, true);
            } else {
                i += 1;
            }
        }
    }

    /// Copy/cut `paths`: the internal file clipboard (dims cut rows) and the system clipboard
    /// (`text/uri-list` or `x-special/gnome-copied-files`), so other programs can paste them.
    pub fn set_file_clip(&mut self, paths: Vec<PathBuf>, cut: bool) {
        self.clip.set_files(&crate::clipboard::FileList { paths: paths.clone(), cut });
        self.file_clip = FileClip { paths, cut };
    }

    /// What Ctrl+V pastes: the files on the system clipboard (copied here or in another program),
    /// else the internal clipboard. The system flavor `text/uri-list` cannot say "cut", so when it
    /// is the very list this app cut the internal flag is kept.
    pub fn paste_source(&mut self) -> FileClip {
        let internal = self.file_clip.clone();
        match self.clip.get_files() {
            Some(list) => {
                let paths: Vec<PathBuf> = list.paths.iter().filter(|p| p.exists()).cloned().collect();
                if paths.is_empty() {
                    internal
                } else if paths == internal.paths {
                    FileClip { paths, cut: internal.cut || list.cut }
                } else {
                    FileClip { paths, cut: list.cut }
                }
            }
            None => internal,
        }
    }

    /// Paste the file clipboard into `dir`.
    pub fn paste_files(&mut self, dir: &Path) {
        let clip = self.paste_source();
        if clip.paths.is_empty() {
            return self.say("nothing to paste");
        }
        let done = self.transfer(&clip.paths, dir, clip.cut);
        if clip.cut && !done.is_empty() {
            self.file_clip = FileClip::default();
        }
    }

    /// Open `path` in the pane that is not focused (needs the split editor).
    fn open_beside(&mut self, path: &Path) {
        let Some(prev) = self.split.as_ref().map(|_| self.active) else {
            return self.say("split the editor first (Ctrl+\\)");
        };
        let Some(idx) = self.open_path(path, None) else { return };
        self.activate(prev);
        self.split_set_other_doc(idx);
        self.request_redraw();
    }

    /// Left mouse on a tab's close slot / middle click: pinned tabs unpin instead of closing.
    pub fn tab_close_click(&mut self, i: usize, button: MouseButton) {
        if self.docs.get(i).is_some_and(|d| d.pinned) {
            if button == MouseButton::Left {
                self.docs[i].pinned = false;
                self.say("tab unpinned");
            } else {
                self.say("tab is pinned — unpin it to close");
            }
            return;
        }
        self.close_tab(i, false);
    }

    // ── drawing ──

    pub fn draw_ctxmenu(&mut self, cv: &mut Canvas) {
        let mut m = match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Ctx(m) => m,
            o => {
                self.overlay = o;
                return;
            }
        };
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        // Hover / keyboard selection: a faint tint of the foreground, the text keeps its colour.
        let tint = fg.with_alpha(0x1c);
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);

        if m.unsaved.is_some() {
            self.draw_unsaved(cv, &mut m);
            self.overlay = Overlay::Ctx(m);
            return;
        }
        if m.ask.is_some() {
            self.draw_ask(cv, &mut m);
            self.overlay = Overlay::Ctx(m);
            return;
        }
        if let Some(cf) = m.confirm.as_mut() {
            let name = crate::app_explorer::describe(&cf.paths);
            let many = cf.paths.len() > 1;
            let w = (sw - 40).clamp(280, 400);
            let r = Rect::new((sw - w) / 2, 70, w, 124);
            cv.shadow(r, 12);
            cv.card(r, 12, c.widget_bg, c.border);
            let title = clip_text(
                &mut self.tx,
                &if many { format!("Delete {name}?") } else { format!("Delete “{name}”?") },
                r.w - 32,
            );
            self.tx.draw_ui(cv, r.x + 16, r.y + 14, &title, fg, r, true);
            let what = if many {
                "The items (and everything inside folders) will be deleted."
            } else if cf.paths.first().is_some_and(|p| p.is_dir()) {
                "The folder and everything in it will be deleted."
            } else {
                "The file will be deleted."
            };
            self.tx.draw_ui(cv, r.x + 16, r.y + 40, what, dim, r, false);
            self.tx.draw_ui(cv, r.x + 16, r.y + 60, "This cannot be undone.", dim, r, false);
            let (bw, bh) = (72, 30);
            let by = r.bottom() - bh - 12;
            let yes_r = Rect::new(r.right() - 12 - bw, by, bw, bh);
            let no_r = Rect::new(yes_r.x - 8 - bw, by, bw, bh);
            for (rect, label, is_yes) in [(no_r, "No", false), (yes_r, "Yes", true)] {
                let focused = cf.yes == is_yes;
                cv.card(rect, 8, c.widget_bg, c.border);
                if focused {
                    cv.fill_round(rect.inset(1), 7, tint);
                }
                let col = if is_yes { c.error } else { fg };
                let tw = self.tx.ui_width(label, false);
                self.tx.draw_ui(cv, rect.x + (rect.w - tw) / 2, rect.y + (bh - 18) / 2, label, col, rect, focused);
                self.layout.overlay_rows.push((rect, is_yes as usize));
            }
            cf.rects = [no_r, yes_r];
            m.card = r;
            self.overlay = Overlay::Ctx(m);
            return;
        }

        // Menu geometry.
        let mut content_w = 0;
        for e in &m.entries {
            if let Entry::Item { label, key, .. } = e {
                let lw = self.tx.ui_width(label, false);
                let kw = if key.is_empty() { 0 } else { self.tx.ui_width(key, false) + 28 };
                content_w = content_w.max(lw + kw);
            }
        }
        let w = (content_w + 40 + 16 + 2 * PAD).clamp(200, 360);
        let h = 2 * PAD + m.entries.iter().map(|e| if *e == Entry::Sep { SEP_H } else { ITEM_H }).sum::<i32>();
        let x = m.at.0.min(sw - w - 8).max(8);
        let y = if m.at.1 + h <= sh - 8 {
            m.at.1
        } else if m.at.1 - h >= 8 {
            m.at.1 - h
        } else {
            (sh - h - 8).max(8)
        };
        let r = Rect::new(x, y, w, h);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);

        let mut rects = Vec::new();
        let mut cy = r.y + PAD;
        for (i, e) in m.entries.iter().enumerate() {
            match e {
                Entry::Sep => {
                    cv.fill_clip(Rect::new(r.x + PAD + 4, cy + SEP_H / 2, r.w - 2 * PAD - 8, 1), c.border, r);
                    cy += SEP_H;
                }
                Entry::Item { label, icon, key, danger, .. } => {
                    let rr = Rect::new(r.x + PAD, cy, r.w - 2 * PAD, ITEM_H);
                    if m.sel == Some(i) {
                        cv.fill_round(rr, 7, tint);
                    }
                    let col = if *danger { c.error } else { fg };
                    self.tx.icon(
                        cv,
                        icon,
                        rr.x + 10,
                        cy + (ITEM_H - 16) / 2,
                        16,
                        if *danger { c.error } else { dim },
                        rr,
                    );
                    self.tx.draw_ui(cv, rr.x + 36, cy + (ITEM_H - 18) / 2, label, col, rr, false);
                    if !key.is_empty() {
                        let kw = self.tx.ui_width(key, false);
                        self.tx.draw_ui(cv, rr.right() - 10 - kw, cy + (ITEM_H - 18) / 2, key, dim, rr, false);
                    }
                    rects.push((rr, i));
                    self.layout.overlay_rows.push((rr, i));
                    cy += ITEM_H;
                }
            }
        }
        m.rects = rects;
        m.card = r;
        self.overlay = Overlay::Ctx(m);
    }
}

fn clip_text(tx: &mut crate::ui::TextCtx, s: &str, max: i32) -> String {
    if tx.ui_width(s, true) <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().collect();
    while out.chars().count() > 1 && tx.ui_width(&format!("{out}…”?"), true) > max {
        out.pop();
    }
    format!("{out}…”?")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let base = std::env::var_os("HOME")
            .map(|h| Path::new(&h).join(".cache/lumen-agents"))
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("ctxmenu-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_split_and_stay_unique() {
        assert_eq!(split_name("a.ts"), ("a", ".ts"));
        assert_eq!(split_name(".env"), (".env", ""));
        assert_eq!(split_name("x.tar.gz"), ("x", ".tar.gz"));
        assert_eq!(split_name("Makefile"), ("Makefile", ""));
        let d = scratch("names");
        assert_eq!(unique_dest(&d, "a.ts"), d.join("a.ts"));
        fs::write(d.join("a.ts"), "1").unwrap();
        assert_eq!(unique_dest(&d, "a.ts"), d.join("a (copy).ts"));
        fs::write(d.join("a (copy).ts"), "2").unwrap();
        assert_eq!(unique_dest(&d, "a.ts"), d.join("a (copy 2).ts"));
        // Duplicating a copy does not stack suffixes.
        assert_eq!(unique_dest(&d, "a (copy).ts"), d.join("a (copy 2).ts"));
        fs::write(d.join(".env"), "").unwrap();
        assert_eq!(unique_dest(&d, ".env"), d.join(".env (copy)"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn copy_dir_and_paste_rules() {
        let d = scratch("paste");
        fs::create_dir_all(d.join("src/deep")).unwrap();
        fs::write(d.join("src/a.ts"), "A").unwrap();
        fs::write(d.join("src/deep/b.ts"), "B").unwrap();
        fs::create_dir_all(d.join("dest")).unwrap();
        // Copy a folder tree.
        let to = paste_one(&d.join("src"), &d.join("dest"), false).unwrap().unwrap();
        assert_eq!(to, d.join("dest/src"));
        assert_eq!(fs::read_to_string(d.join("dest/src/deep/b.ts")).unwrap(), "B");
        assert!(d.join("src/a.ts").exists(), "copy keeps the source");
        // Pasting again picks a unique name.
        let again = paste_one(&d.join("src"), &d.join("dest"), false).unwrap().unwrap();
        assert_eq!(again, d.join("dest/src (copy)"));
        // Move a file: source disappears, existing name gets a unique one.
        fs::write(d.join("dest/a.ts"), "old").unwrap();
        let moved = paste_one(&d.join("src/a.ts"), &d.join("dest"), true).unwrap().unwrap();
        assert_eq!(moved, d.join("dest/a (copy).ts"));
        assert!(!d.join("src/a.ts").exists());
        assert_eq!(fs::read_to_string(d.join("dest/a.ts")).unwrap(), "old");
        // Cut into the same folder: nothing to do.
        assert_eq!(paste_one(&d.join("dest/a.ts"), &d.join("dest"), true).unwrap(), None);
        // A folder cannot go into itself.
        assert!(paste_one(&d.join("src"), &d.join("src/deep"), false).is_err());
        assert!(paste_one(&d.join("src"), &d.join("src"), true).is_err());
        // Missing source.
        assert!(paste_one(&d.join("nope"), &d.join("dest"), false).is_err());
        // Duplicate = copy into the own folder.
        let dup = paste_one(&d.join("dest/a.ts"), &d.join("dest"), false).unwrap().unwrap();
        assert_eq!(dup, d.join("dest/a (copy 2).ts"));
        fs::remove_dir_all(&d).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_copied_as_links() {
        let d = scratch("link");
        fs::write(d.join("t.txt"), "t").unwrap();
        std::os::unix::fs::symlink("t.txt", d.join("l")).unwrap();
        copy_recursive(&d.join("l"), &d.join("l2")).unwrap();
        assert!(fs::symlink_metadata(d.join("l2")).unwrap().file_type().is_symlink());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    #[cfg(unix)]
    fn trash_fallback_writes_info_and_keeps_names_unique() {
        let d = scratch("trash");
        let trash = d.join("Trash");
        fs::write(d.join("my file.txt"), "x").unwrap();
        let t1 = trash_into(&d.join("my file.txt"), &trash).unwrap();
        assert_eq!(t1, trash.join("files/my file.txt"));
        assert!(!d.join("my file.txt").exists());
        let info = fs::read_to_string(trash.join("info/my file.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
        assert!(info.contains(&format!("Path={}/my%20file.txt\n", pct_encode(&d.to_string_lossy()))), "{info}");
        assert!(info.contains("DeletionDate=20"), "{info}");
        // A second file with the same name does not overwrite the first.
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub/inner.txt"), "i").unwrap();
        fs::write(d.join("my file.txt"), "y").unwrap();
        let t2 = trash_into(&d.join("my file.txt"), &trash).unwrap();
        assert_eq!(t2, trash.join("files/my file (copy).txt"));
        assert_eq!(fs::read_to_string(&t1).unwrap(), "x");
        assert!(trash.join("info/my file (copy).txt.trashinfo").exists());
        // Folders go whole.
        let t3 = trash_into(&d.join("sub"), &trash).unwrap();
        assert!(t3.join("inner.txt").exists() && !d.join("sub").exists());
        assert_eq!(pct_encode("a b/ç"), "a%20b/%C3%A7");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn pinned_tabs_come_first() {
        assert_eq!(pinned_first(&[false, true, false, true]), vec![1, 3, 0, 2]);
        assert_eq!(pinned_first(&[false, false]), vec![0, 1]);
    }

    #[test]
    fn cut_rows_are_dimmed_until_cleared() {
        let clip = FileClip { paths: vec![PathBuf::from("/p/src"), PathBuf::from("/p/a.ts")], cut: true };
        assert!(clip.is_cut(Path::new("/p/a.ts")));
        assert!(clip.is_cut(Path::new("/p/src/deep/b.ts")), "children of a cut folder are cut too");
        assert!(!clip.is_cut(Path::new("/p/src2/b.ts")), "a sibling that merely shares the prefix is not");
        assert!(!FileClip { cut: false, ..clip.clone() }.is_cut(Path::new("/p/a.ts")), "copied rows are not dimmed");
        assert!(!FileClip::default().is_cut(Path::new("/p/a.ts")));
    }

    fn labels(v: &[Entry]) -> Vec<&'static str> {
        v.iter().map(|e| if let Entry::Item { label, .. } = e { *label } else { "-" }).collect()
    }

    #[test]
    fn menus_have_the_right_items() {
        let folder = Target::Folder("/p/src".into());
        let f = Flags::default();
        assert_eq!(
            labels(&entries_for(&folder, &f)),
            ["New File", "New Folder", "-", "Cut", "Copy", "-", "Duplicate", "Rename", "-", "Move to Trash", "Delete"]
        );
        let with_clip = Flags { has_clip: true, ..f };
        assert!(labels(&entries_for(&folder, &with_clip)).contains(&"Paste"));
        let file = Target::File("/p/a.ts".into());
        let l = labels(&entries_for(&file, &f));
        assert_eq!(
            l,
            [
                "Cut",
                "Copy",
                "-",
                "Duplicate",
                "Rename",
                "-",
                "Move to Trash",
                "Delete",
                "-",
                "Copy Path",
                "Local History"
            ]
        );
        assert!(!l.contains(&"Open to the Side") && !l.contains(&"Paste"));
        assert!(labels(&entries_for(&file, &Flags { split: true, ..f })).contains(&"Open to the Side"));
        let tab = Target::Tab(0);
        let l = labels(&entries_for(&tab, &Flags { has_path: true, ..f }));
        assert_eq!(
            l,
            [
                "Close",
                "Close Others",
                "Close All",
                "-",
                "Pin Tab",
                "Move to New Window",
                "-",
                "Copy Path",
                "Local History"
            ]
        );
        let l = labels(&entries_for(&tab, &Flags { has_path: true, pinned: true, md: Some(false), ..f }));
        assert!(l.contains(&"Unpin Tab") && l.contains(&"Open Markdown Preview"));
        assert!(labels(&entries_for(&tab, &Flags { md: Some(true), ..f })).contains(&"Close Markdown Preview"));
        // No path (untitled): no separator left dangling.
        let l = labels(&entries_for(&tab, &f));
        assert_eq!(l.last(), Some(&"Move to New Window"));
        // Something was closed: "Reopen Closed Tab" follows the close items and runs a command.
        let v = entries_for(&tab, &Flags { reopen: true, ..f });
        assert_eq!(labels(&v)[..5], ["Close", "Close Others", "Close All", "Reopen Closed Tab", "-"]);
        assert!(
            matches!(v[3], Entry::Item { act: Act::Run(id), .. } if crate::app_overlay::Cmd::from_id(id).is_some())
        );
        // Root: only creation (+ paste).
        assert_eq!(labels(&entries_for(&Target::Root("/p".into()), &f)), ["New File", "New Folder"]);
        // Several rows: no rename / new / open-beside, plural "Copy Paths".
        let multi = Target::Multi(vec!["/p/a.ts".into(), "/p/b.ts".into()]);
        assert_eq!(
            labels(&entries_for(&multi, &f)),
            ["Cut", "Copy", "-", "Duplicate", "-", "Move to Trash", "Delete", "-", "Copy Paths"]
        );
        // The editor's text: the agent first, then the language server's actions.
        let ed = entries_for(&Target::Editor, &f);
        assert_eq!(
            labels(&ed),
            [
                "Send Selection to Agent",
                "-",
                "Go to Definition",
                "Find References",
                "Rename Symbol",
                "-",
                "Format Document"
            ]
        );
        // Every editor entry runs a command that exists.
        for e in &ed {
            if let Entry::Item { act: Act::Run(id), .. } = e {
                assert!(crate::app_overlay::Cmd::from_id(id).is_some(), "{id}");
            }
        }
    }

    #[test]
    fn moving_several_items_applies_the_unique_name_rule_to_each() {
        let d = scratch("multi");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("dest")).unwrap();
        for f in ["a.ts", "b.ts"] {
            fs::write(d.join("src").join(f), f).unwrap();
        }
        fs::write(d.join("dest/a.ts"), "old").unwrap();
        let moved: Vec<PathBuf> = [d.join("src/a.ts"), d.join("src/b.ts")]
            .iter()
            .filter_map(|p| paste_one(p, &d.join("dest"), true).unwrap())
            .collect();
        assert_eq!(moved, [d.join("dest/a (copy).ts"), d.join("dest/b.ts")]);
        assert!(!d.join("src/a.ts").exists() && !d.join("src/b.ts").exists());
        assert_eq!(fs::read_to_string(d.join("dest/a.ts")).unwrap(), "old");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn keyboard_steps_skip_separators_and_wrap() {
        let e = entries_for(&Target::Tab(0), &Flags::default());
        // Items at 0,1,2, sep 3, items 4,5.
        assert_eq!(step(&e, None, 1), Some(0));
        assert_eq!(step(&e, None, -1), Some(5));
        assert_eq!(step(&e, Some(2), 1), Some(4));
        assert_eq!(step(&e, Some(5), 1), Some(0));
        assert_eq!(step(&e, Some(0), -1), Some(5));
        assert_eq!(step(&[], None, 1), None);
    }

    // ---- App-level: Esc cancels a cut from the editor; the system clipboard carries files ----

    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    fn key(app: &mut App, k: winit::keyboard::NamedKey) {
        app.handle_key(&crate::app::KeyIn { key: winit::keyboard::Key::Named(k), text: None });
    }

    #[test]
    fn esc_cancels_a_pending_cut_from_the_editor_too() {
        let _g = lock();
        let dir = tmp("esccut");
        let mut app = test_app(&dir);
        let f = dir.join("seed.txt");
        app.set_file_clip(vec![f.clone()], true);
        assert!(app.file_clip.is_cut(&f));
        // Focus is in the editor, not the explorer.
        app.focus = crate::app::Focus::Editor;
        key(&mut app, winit::keyboard::NamedKey::Escape);
        assert!(app.file_clip.paths.is_empty(), "the cut is forgotten (rows stop being dimmed)");
        // A copy is not a cut: Esc leaves it alone.
        app.set_file_clip(vec![f.clone()], false);
        key(&mut app, winit::keyboard::NamedKey::Escape);
        assert_eq!(app.file_clip.paths, vec![f.clone()]);
        // Esc that closes something else (extra cursors) does not eat the cut.
        app.set_file_clip(vec![f.clone()], true);
        app.docs[0].ed.extra.push(crate::buffer::Sel::caret(1));
        key(&mut app, winit::keyboard::NamedKey::Escape);
        assert!(app.docs[0].ed.extra.is_empty() && app.file_clip.cut, "first Esc only drops the extra cursors");
        key(&mut app, winit::keyboard::NamedKey::Escape);
        assert!(!app.file_clip.cut, "the next one forgets the cut");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn wait_for(path: &Path, what: &str) -> String {
        for _ in 0..200 {
            if let Some(s) = std::fs::read_to_string(path).ok().filter(|s| !s.is_empty()) {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{what}: nothing arrived");
    }

    #[cfg(unix)]
    #[test]
    fn files_travel_through_the_system_clipboard_in_both_directions() {
        let _g = lock();
        let dir = tmp("sysclip");
        let mut app = test_app(&dir);
        let fake = dir.join("fake");
        app.clip = crate::clipboard::Clipboard::with_fake_tools(&fake);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "A").unwrap();
        // Copy here: the system clipboard now carries the file for other programs...
        app.set_file_clip(vec![dir.join("a.txt")], false);
        let data = wait_for(&fake.join("data"), "copy");
        assert!(data.contains(&format!("file://{}/a.txt", dir.display())), "{data:?}");
        // ...and pasting reads it back (the same list, so the internal flag is kept).
        app.paste_files(&dir.join("sub"));
        assert_eq!(std::fs::read_to_string(dir.join("sub/a.txt")).unwrap(), "A");
        assert!(dir.join("a.txt").exists(), "a copy leaves the source");

        // Another program copies a file whose name needs escaping; our own clipboard knows nothing.
        std::fs::write(dir.join("o ther é.txt"), "O").unwrap();
        app.file_clip = FileClip::default();
        std::fs::write(fake.join("mime"), "text/uri-list\n").unwrap();
        std::fs::write(fake.join("data"), format!("file://{}/o%20ther%20%C3%A9.txt\r\n", dir.display())).unwrap();
        assert!(app.paste_source().paths == vec![dir.join("o ther é.txt")]);
        app.paste_files(&dir.join("sub"));
        assert_eq!(std::fs::read_to_string(dir.join("sub/o ther é.txt")).unwrap(), "O");
        assert!(dir.join("o ther é.txt").exists());

        // A file manager's "cut" (GNOME flavor) moves the file.
        std::fs::write(dir.join("m.txt"), "M").unwrap();
        std::fs::write(fake.join("mime"), "x-special/gnome-copied-files\n").unwrap();
        std::fs::write(fake.join("data"), format!("cut\nfile://{}/m.txt", dir.display())).unwrap();
        app.paste_files(&dir.join("sub"));
        assert!(dir.join("sub/m.txt").exists() && !dir.join("m.txt").exists());

        // Files that vanished are skipped; with nothing left the internal clipboard answers.
        std::fs::write(fake.join("data"), "copy\nfile:///no/such/file").unwrap();
        app.file_clip = FileClip { paths: vec![dir.join("a.txt")], cut: true };
        assert_eq!(app.paste_source().paths, vec![dir.join("a.txt")]);

        // What this app cut keeps its "cut" even through a flavor that cannot say so.
        app.set_file_clip(vec![dir.join("a.txt")], true);
        wait_for(&fake.join("data"), "cut");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(app.paste_source().cut);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
