//! Context menus (right click): sidebar folders and files, editor tabs.
//!
//! The menu is an [`Overlay::Ctx`]: a floating rounded card with a soft shadow, thin separators,
//! a HugeIcons/Zed icon and the shortcut (when there is one) on the right of every item. The
//! hover (and the keyboard selection, same look) is a low-opacity tint of the foreground, so the
//! text keeps its colour and stays readable on any theme. Arrows + Enter, Esc or a click outside
//! close/accept it. "Excluir" asks for confirmation with a small Sim/Não card (same overlay).
//!
//! File operations (cut/copy/paste/duplicate/trash) are plain functions at the top of the file
//! (unit tested); the `impl App` part wires them to the tree, open tabs and the watcher.
//! Pinned tabs are a flag on [`Doc`]: they are drawn first (display order only, so document
//! indices never move), show a pin instead of the close button and survive "Fechar todas".

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

/// "foo (cópia 3)" -> "foo".
fn strip_copy_suffix(stem: &str) -> &str {
    if let Some(open) = stem.rfind(" (cópia") {
        let tail = &stem[open + " (cópia".len()..];
        let ok = tail == ")" || (tail.starts_with(' ') && tail.ends_with(')') && tail[1..tail.len() - 1].chars().all(|c| c.is_ascii_digit()) && tail.len() > 2);
        if ok {
            return &stem[..open];
        }
    }
    stem
}

fn taken(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// `dir/name`, or `dir/stem (cópia).ext`, `dir/stem (cópia 2).ext`… when something is already there.
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
        let cand = if n == 1 { format!("{stem} (cópia){ext}") } else { format!("{stem} (cópia {n}){ext}") };
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
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
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
        return Err(format!("{} não existe mais", from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    if from.is_dir() && dir.starts_with(from) {
        return Err("não dá para colar uma pasta dentro dela mesma".into());
    }
    if cut && from.parent() == Some(dir) {
        return Ok(None);
    }
    let name = from.file_name().ok_or("caminho inválido")?.to_string_lossy().into_owned();
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
    // SAFETY: `localtime_r` only writes into the zeroed `tm` we own.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// Freedesktop trash fallback: move `path` into `<trash>/files` and write `<trash>/info/*.trashinfo`.
pub fn trash_into(path: &Path, trash: &Path) -> io::Result<PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let name = abs.file_name().ok_or_else(|| io::Error::other("caminho inválido"))?.to_string_lossy().into_owned();
    let info_of = |p: &Path| info.join(format!("{}.trashinfo", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    let dest = unique_dest_by(&files, &name, |p| taken(p) || taken(&info_of(p)));
    let info_file = info_of(&dest);
    fs::write(&info_file, format!("[Trash Info]\nPath={}\nDeletionDate={}\n", pct_encode(&abs.to_string_lossy()), local_stamp()))?;
    if let Err(e) = move_path(&abs, &dest) {
        let _ = fs::remove_file(&info_file);
        return Err(e);
    }
    Ok(dest)
}

fn user_trash() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))?;
    Some(base.join("Trash"))
}

/// Move to the trash: `gio trash`, else the freedesktop trash by hand.
pub fn trash_path(path: &Path) -> Result<(), String> {
    let ok = Command::new("gio").arg("trash").arg("--").arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if ok && !taken(path) {
        return Ok(());
    }
    let trash = user_trash().ok_or("sem pasta da lixeira")?;
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

/// Tabs "Fechar outras" (`keep = Some(i)`) / "Fechar todas" (`None`) close, highest index first.
/// Pinned tabs stay; unsaved ones are skipped and counted.
pub fn tabs_to_close(pinned: &[bool], dirty: &[bool], keep: Option<usize>) -> (Vec<usize>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for i in (0..pinned.len()).rev() {
        if Some(i) == keep || pinned[i] {
            continue;
        }
        if dirty[i] {
            skipped += 1;
        } else {
            out.push(i);
        }
    }
    (out, skipped)
}

// ───────────────────────── menu model ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Folder(PathBuf),
    File(PathBuf),
    /// Empty area of the explorer: the project root.
    Root(PathBuf),
    /// Document index.
    Tab(usize),
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
    let trash = || item(Act::Trash, "Mover para a lixeira", "hugeicons/trash", "");
    let delete = || Entry::Item { act: Act::Delete, label: "Excluir", icon: "ui/delete", key: "Del", danger: true };
    let v = match t {
        Target::Folder(_) | Target::Root(_) => {
            let mut v = vec![item(Act::NewFile, "Novo arquivo", "hugeicons/plus", "A"), item(Act::NewFolder, "Nova pasta", "ui/folder_add", "Shift+A"), sep()];
            if matches!(t, Target::Folder(_)) {
                v.extend([item(Act::Cut, "Recortar", "ui/scissors", ""), item(Act::Copy, "Copiar", "hugeicons/copy", "")]);
            }
            if f.has_clip {
                v.push(item(Act::Paste, "Colar", "hugeicons/download", ""));
            }
            if matches!(t, Target::Folder(_)) {
                v.extend([sep(), item(Act::Duplicate, "Duplicar", "ui/file_multiple", ""), item(Act::Rename, "Renomear", "hugeicons/edit", "F2"), sep(), trash(), delete()]);
            }
            v
        }
        Target::File(_) => {
            let mut v = vec![
                item(Act::Cut, "Recortar", "ui/scissors", ""),
                item(Act::Copy, "Copiar", "hugeicons/copy", ""),
                sep(),
                item(Act::Duplicate, "Duplicar", "ui/file_multiple", ""),
                item(Act::Rename, "Renomear", "hugeicons/edit", "F2"),
                sep(),
                trash(),
                delete(),
                sep(),
                item(Act::CopyPath, "Copiar caminho", "hugeicons/link", ""),
            ];
            if f.split {
                v.push(item(Act::OpenSide, "Abrir ao lado", "ui/split", ""));
            }
            v
        }
        Target::Tab(_) => {
            let mut v = vec![
                item(Act::CloseTab, "Fechar", "ui/close", "Ctrl+W"),
                item(Act::CloseOthers, "Fechar outras", "ui/x_circle", ""),
                item(Act::CloseAll, "Fechar todas", "ui/list_x", ""),
                sep(),
                if f.pinned { item(Act::TogglePin, "Desafixar aba", "ui/unpin", "") } else { item(Act::TogglePin, "Fixar aba", "ui/pin", "") },
                sep(),
            ];
            if f.has_path {
                v.push(item(Act::CopyPath, "Copiar caminho", "hugeicons/link", ""));
            }
            if let Some(open) = f.md {
                v.push(item(Act::Markdown, if open { "Fechar Markdown preview" } else { "Abrir Markdown preview" }, "ui/file_markdown", "Ctrl+Shift+V"));
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
    pub path: PathBuf,
    /// Focused button: true = "Sim". Starts on "Não".
    pub yes: bool,
    /// Buttons ("Não", "Sim") as drawn last frame.
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
                return None;
            }
            let rel = y - (sb.y + TAB_H) as f64 + self.tree.scroll;
            let row = (rel >= 0.0).then(|| (rel / ROW_H as f64) as usize).filter(|&i| i < self.tree.rows.len());
            return Some(match row {
                Some(i) => {
                    self.tree.sel = i;
                    self.focus = Focus::Sidebar;
                    let r = &self.tree.rows[i];
                    if r.is_dir { Target::Folder(r.path.clone()) } else { Target::File(r.path.clone()) }
                }
                None => Target::Root(self.root.clone()),
            });
        }
        None
    }

    pub fn open_ctxmenu(&mut self, target: Target, at: (i32, i32)) {
        let mut f = Flags { has_clip: !self.file_clip.paths.is_empty(), split: self.split.is_some(), ..Flags::default() };
        if let Target::Tab(i) = &target {
            let Some(d) = self.docs.get(*i) else { return };
            f.pinned = d.pinned;
            f.has_path = d.path().is_some();
            f.md = crate::app_md::is_markdown_name(&d.name).then(|| d.md.is_some());
        }
        let entries = entries_for(&target, &f);
        self.overlay = Overlay::Ctx(Box::new(CtxMenu { target, entries, at, sel: None, rects: Vec::new(), card: Rect::default(), confirm: None }));
    }

    pub fn ctxmenu_key(&mut self, ev: &crate::app::KeyIn) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        if let Some(c) = m.confirm.as_mut() {
            match &ev.key {
                Key::Named(NamedKey::Escape) => self.overlay = Overlay::None,
                Key::Named(NamedKey::ArrowLeft) => c.yes = false,
                Key::Named(NamedKey::ArrowRight) => c.yes = true,
                Key::Named(NamedKey::Tab) => c.yes = !c.yes,
                Key::Named(NamedKey::Enter) => {
                    let (yes, path) = (c.yes, c.path.clone());
                    self.overlay = Overlay::None;
                    if yes {
                        self.delete_path(&path);
                    } else {
                        self.say("exclusão cancelada");
                    }
                }
                Key::Character(s) if matches!(s.to_lowercase().as_str(), "s" | "y") => {
                    let path = c.path.clone();
                    self.overlay = Overlay::None;
                    self.delete_path(&path);
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
        if let Some(c) = m.confirm.as_ref() {
            let (no, yes, path) = (c.rects[0], c.rects[1], c.path.clone());
            if yes.contains(x, y) {
                self.overlay = Overlay::None;
                self.delete_path(&path);
            } else if no.contains(x, y) || !m.card.contains(x, y) {
                self.overlay = Overlay::None;
                self.say("exclusão cancelada");
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
        let (x, y) = self.mouse;
        let Overlay::Ctx(m) = &mut self.overlay else { return false };
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
            Target::Tab(i) => self.docs.get(*i).and_then(|d| d.path()).map(Path::to_path_buf),
        }
    }

    fn ctx_run(&mut self, act: Act, t: Target) {
        let path = self.ctx_path(&t);
        let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match act {
            Act::NewFile | Act::NewFolder => {
                let Some(dir) = path else { return };
                let is_dir = act == Act::NewFolder;
                let rel = self.rel(&dir);
                let title = format!("{} em {}", if is_dir { "Nova pasta" } else { "Novo arquivo" }, if rel.is_empty() { "." } else { &rel });
                let kind = if is_dir { PromptKind::NewDir { dir } } else { PromptKind::NewFile { dir } };
                self.overlay = Overlay::Prompt(Prompt { kind, title, field: TextField::default() });
            }
            Act::Cut | Act::Copy => {
                let Some(p) = path else { return };
                let cut = act == Act::Cut;
                self.file_clip = FileClip { paths: vec![p], cut };
                self.say(format!("{}: {name}", if cut { "recortado" } else { "copiado" }));
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
                        self.say(format!("duplicado: {}", to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    }
                    Ok(None) => {}
                    Err(e) => self.say(format!("erro ao duplicar: {e}")),
                }
            }
            Act::Rename => {
                let Some(p) = path else { return };
                self.overlay = Overlay::Prompt(Prompt { title: format!("Renomear {name}"), kind: PromptKind::RenamePath { from: p }, field: TextField::with(&name) });
            }
            Act::Trash => {
                let Some(p) = path else { return };
                match trash_path(&p) {
                    Ok(()) => {
                        self.close_docs_under(&p);
                        self.refresh_tree();
                        self.watch_dirs();
                        self.say(format!("movido para a lixeira: {name}"));
                    }
                    Err(e) => self.say(format!("erro ao mover para a lixeira: {e}")),
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
                    confirm: Some(Confirm { path: p, yes: false, rects: [Rect::default(); 2] }),
                }));
            }
            Act::CopyPath => {
                if let Some(p) = path {
                    self.clip.set(&p.to_string_lossy());
                    self.say("caminho copiado");
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
                let pinned: Vec<bool> = self.docs.iter().map(|d| d.pinned).collect();
                let dirty: Vec<bool> = self.docs.iter().map(|d| d.buf.is_dirty()).collect();
                let (list, skipped) = tabs_to_close(&pinned, &dirty, keep);
                let kept_idx = keep.map(|k| k - list.iter().filter(|&&i| i < k).count());
                for i in list {
                    self.close_tab(i, true);
                }
                if let Some(k) = kept_idx.filter(|&k| k < self.docs.len()) {
                    // "Fechar outras": the tab the menu was opened on is the one left in front.
                    self.activate(k);
                }
                if skipped > 0 {
                    self.say(format!("{skipped} aba(s) com alterações não salvas foram mantidas"));
                }
            }
            Act::TogglePin => {
                if let Target::Tab(i) = t {
                    if let Some(d) = self.docs.get_mut(i) {
                        d.pinned = !d.pinned;
                        let msg = if d.pinned { format!("aba fixada: {}", d.name) } else { format!("aba desafixada: {}", d.name) };
                        self.say(msg);
                    }
                }
            }
            Act::Markdown => {
                if let Target::Tab(i) = t {
                    self.activate(i);
                    self.toggle_md_reader();
                }
            }
        }
    }

    fn after_fs_change(&mut self, reveal: &Path) {
        self.refresh_tree();
        self.tree.reveal(reveal);
        self.watch_dirs();
    }

    fn close_docs_under(&mut self, path: &Path) {
        let mut i = 0;
        while i < self.docs.len() {
            if self.docs[i].path().is_some_and(|p| p.starts_with(path)) {
                self.close_tab(i, true);
            } else {
                i += 1;
            }
        }
    }

    /// Paste the file clipboard into `dir`.
    pub fn paste_files(&mut self, dir: &Path) {
        let clip = self.file_clip.clone();
        if clip.paths.is_empty() {
            return self.say("nada para colar");
        }
        let (mut done, mut last, mut err) = (0, None, None);
        for from in &clip.paths {
            match paste_one(from, dir, clip.cut) {
                Ok(Some(to)) => {
                    if clip.cut {
                        self.repoint_docs(from, &to);
                    }
                    done += 1;
                    last = Some(to);
                }
                Ok(None) => {}
                Err(e) => err = Some(e),
            }
        }
        if clip.cut && done > 0 {
            self.file_clip = FileClip::default();
        }
        if let Some(to) = &last {
            self.after_fs_change(to);
        }
        match (err, done) {
            (Some(e), _) => self.say(format!("erro ao colar: {e}")),
            (None, 0) => self.say("já está nesta pasta"),
            (None, n) => self.say(format!("{n} item(ns) {}", if clip.cut { "movido(s)" } else { "colado(s)" })),
        }
    }

    /// Open `path` in the pane that is not focused (needs the split editor).
    fn open_beside(&mut self, path: &Path) {
        let Some(prev) = self.split.as_ref().map(|_| self.active) else { return self.say("divida o editor primeiro (Ctrl+\\)") };
        let Some(idx) = self.open_path(path, None) else { return };
        self.activate(prev);
        if let Some(sp) = self.split.as_mut() {
            sp.other = idx;
            sp.view = Default::default();
        }
        self.request_redraw();
    }

    /// Left mouse on a tab's close slot / middle click: pinned tabs unpin instead of closing.
    pub fn tab_close_click(&mut self, i: usize, button: MouseButton) {
        if self.docs.get(i).is_some_and(|d| d.pinned) {
            if button == MouseButton::Left {
                self.docs[i].pinned = false;
                self.say("aba desafixada");
            } else {
                self.say("aba fixada — desafixe para fechar");
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

        if let Some(cf) = m.confirm.as_mut() {
            let name = cf.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let w = (sw - 40).clamp(280, 400);
            let r = Rect::new((sw - w) / 2, 70, w, 124);
            cv.shadow(r, 12);
            cv.card(r, 12, c.widget_bg, c.border);
            let title = clip_text(&mut self.tx, &format!("Excluir “{name}”?"), r.w - 32);
            self.tx.draw_ui(cv, r.x + 16, r.y + 14, &title, fg, r, true);
            let what = if cf.path.is_dir() { "A pasta e tudo dentro dela serão apagados." } else { "O arquivo será apagado." };
            self.tx.draw_ui(cv, r.x + 16, r.y + 40, what, dim, r, false);
            self.tx.draw_ui(cv, r.x + 16, r.y + 60, "Isso não pode ser desfeito.", dim, r, false);
            let (bw, bh) = (72, 30);
            let by = r.bottom() - bh - 12;
            let yes_r = Rect::new(r.right() - 12 - bw, by, bw, bh);
            let no_r = Rect::new(yes_r.x - 8 - bw, by, bw, bh);
            for (rect, label, is_yes) in [(no_r, "Não", false), (yes_r, "Sim", true)] {
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
                    self.tx.icon(cv, icon, rr.x + 10, cy + (ITEM_H - 16) / 2, 16, if *danger { c.error } else { dim }, rr);
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
        let base = std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache/lumen-agents")).unwrap_or_else(std::env::temp_dir);
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
        assert_eq!(unique_dest(&d, "a.ts"), d.join("a (cópia).ts"));
        fs::write(d.join("a (cópia).ts"), "2").unwrap();
        assert_eq!(unique_dest(&d, "a.ts"), d.join("a (cópia 2).ts"));
        // Duplicating a copy does not stack suffixes.
        assert_eq!(unique_dest(&d, "a (cópia).ts"), d.join("a (cópia 2).ts"));
        fs::write(d.join(".env"), "").unwrap();
        assert_eq!(unique_dest(&d, ".env"), d.join(".env (cópia)"));
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
        assert_eq!(again, d.join("dest/src (cópia)"));
        // Move a file: source disappears, existing name gets a unique one.
        fs::write(d.join("dest/a.ts"), "old").unwrap();
        let moved = paste_one(&d.join("src/a.ts"), &d.join("dest"), true).unwrap().unwrap();
        assert_eq!(moved, d.join("dest/a (cópia).ts"));
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
        assert_eq!(dup, d.join("dest/a (cópia 2).ts"));
        fs::remove_dir_all(&d).ok();
    }

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
        assert_eq!(t2, trash.join("files/my file (cópia).txt"));
        assert_eq!(fs::read_to_string(&t1).unwrap(), "x");
        assert!(trash.join("info/my file (cópia).txt.trashinfo").exists());
        // Folders go whole.
        let t3 = trash_into(&d.join("sub"), &trash).unwrap();
        assert!(t3.join("inner.txt").exists() && !d.join("sub").exists());
        assert_eq!(pct_encode("a b/ç"), "a%20b/%C3%A7");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn pinned_tabs_come_first_and_survive_close_all() {
        assert_eq!(pinned_first(&[false, true, false, true]), vec![1, 3, 0, 2]);
        assert_eq!(pinned_first(&[false, false]), vec![0, 1]);
        let pinned = [false, true, false, false];
        let dirty = [false, false, true, false];
        // Close all: pinned (1) stays, dirty (2) is skipped, highest index first.
        assert_eq!(tabs_to_close(&pinned, &dirty, None), (vec![3, 0], 1));
        // Close others of tab 3: tab 3 and the pinned one stay.
        assert_eq!(tabs_to_close(&pinned, &dirty, Some(3)), (vec![0], 1));
        // Nothing pinned, keep 0.
        assert_eq!(tabs_to_close(&[false; 3], &[false; 3], Some(0)), (vec![2, 1], 0));
    }

    fn labels(v: &[Entry]) -> Vec<&'static str> {
        v.iter().map(|e| if let Entry::Item { label, .. } = e { *label } else { "-" }).collect()
    }

    #[test]
    fn menus_have_the_right_items() {
        let folder = Target::Folder("/p/src".into());
        let f = Flags::default();
        assert_eq!(labels(&entries_for(&folder, &f)), ["Novo arquivo", "Nova pasta", "-", "Recortar", "Copiar", "-", "Duplicar", "Renomear", "-", "Mover para a lixeira", "Excluir"]);
        let with_clip = Flags { has_clip: true, ..f };
        assert!(labels(&entries_for(&folder, &with_clip)).contains(&"Colar"));
        let file = Target::File("/p/a.ts".into());
        let l = labels(&entries_for(&file, &f));
        assert_eq!(l, ["Recortar", "Copiar", "-", "Duplicar", "Renomear", "-", "Mover para a lixeira", "Excluir", "-", "Copiar caminho"]);
        assert!(!l.contains(&"Abrir ao lado") && !l.contains(&"Colar"));
        assert!(labels(&entries_for(&file, &Flags { split: true, ..f })).contains(&"Abrir ao lado"));
        let tab = Target::Tab(0);
        let l = labels(&entries_for(&tab, &Flags { has_path: true, ..f }));
        assert_eq!(l, ["Fechar", "Fechar outras", "Fechar todas", "-", "Fixar aba", "-", "Copiar caminho"]);
        let l = labels(&entries_for(&tab, &Flags { has_path: true, pinned: true, md: Some(false), ..f }));
        assert!(l.contains(&"Desafixar aba") && l.contains(&"Abrir Markdown preview"));
        assert!(labels(&entries_for(&tab, &Flags { md: Some(true), ..f })).contains(&"Fechar Markdown preview"));
        // No path (untitled): no separator left dangling.
        let l = labels(&entries_for(&tab, &f));
        assert_eq!(l.last(), Some(&"Fixar aba"));
        // Root: only creation (+ paste).
        assert_eq!(labels(&entries_for(&Target::Root("/p".into()), &f)), ["Novo arquivo", "Nova pasta"]);
    }

    #[test]
    fn keyboard_steps_skip_separators_and_wrap() {
        let e = entries_for(&Target::Tab(0), &Flags::default());
        // Items at 0,1,2, sep 3, item 4.
        assert_eq!(step(&e, None, 1), Some(0));
        assert_eq!(step(&e, None, -1), Some(4));
        assert_eq!(step(&e, Some(2), 1), Some(4));
        assert_eq!(step(&e, Some(4), 1), Some(0));
        assert_eq!(step(&e, Some(0), -1), Some(4));
        assert_eq!(step(&[], None, 1), None);
    }
}
