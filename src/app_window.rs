//! Multiple windows: one process per window (see `instance` for the handoff between them).
//!
//! Why processes and not several winit windows in one process: `App` is one window's state
//! through and through (one surface, one layout, one set of documents, overlays and panels), so
//! several windows in one process would mean splitting nearly every field into per-window and
//! shared halves. A process per window keeps every window independent (a crash or a stuck
//! language server takes down one window), costs a few MB of fonts per window, and needs only a
//! small protocol: the per-user instance directory routes files to the right window.
//!
//! * **Slots.** Sessions are saved per project root; the n-th window of the same project (two
//!   windows on one repository) saves to its own slot file (`session-<hash>-<n>.json`, slot 0 keeps
//!   the old name).
//! * **The window set.** `windows.json` lists the windows to reopen when `lumen-text` starts
//!   without arguments: a window adds itself when it opens; closing a window while others stay
//!   open drops it; the last window closed, or every window closed by "Quit" (which closes them
//!   all), stays. Each window's session file holds its tabs and its geometry.
//! * **Moving a tab out** (drag it outside the window, or "Move to New Window"): the tab's path,
//!   caret, scroll and, when it has unsaved changes, its text go to a handoff file written and
//!   synced before the new window is started; the tab closes only once the process is running, and
//!   the new window deletes the file only after opening it. A file nobody took (the new window died
//!   on the way) is reopened by the next window that starts, so unsaved text is never lost. (The
//!   hot-exit backups are per window slot, see [`App::backup_dir`]; the new window's slot is only
//!   known inside the new process, hence the handoff file.)

use crate::{
    app::App,
    instance::{self, Instance, Msg},
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

/// Handoff files older than this were not taken by the window they were meant for.
const ORPHAN_AGE: Duration = Duration::from_secs(30);

/// How `main` launched this window (set before `App::new`, which restores the session with it).
#[derive(Clone, Copy, Debug)]
pub struct Launch {
    pub slot: u32,
    /// Reopen the slot's saved tabs when no file was given (false for a fresh "New window").
    pub restore: bool,
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

pub fn set_launch(l: Launch) {
    let _ = LAUNCH.set(l);
}

/// Window position (desktop pixels, absent where the platform does not tell, e.g. Wayland) and
/// inner size (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub pos: Option<(i32, i32)>,
    pub w: u32,
    pub h: u32,
    pub max: bool,
}

impl Geom {
    pub fn to_json(self) -> Value {
        let mut v = json!({"w": self.w, "h": self.h, "max": self.max});
        if let Some((x, y)) = self.pos {
            v["x"] = json!(x);
            v["y"] = json!(y);
        }
        v
    }

    pub fn from_json(v: &Value) -> Option<Geom> {
        let w = v["w"].as_u64()?.clamp(320, 16_000) as u32;
        let h = v["h"].as_u64()?.clamp(240, 16_000) as u32;
        let pos = match (v["x"].as_i64(), v["y"].as_i64()) {
            (Some(x), Some(y)) => Some((x as i32, y as i32)),
            _ => None,
        };
        Some(Geom { pos, w, h, max: v["max"].as_bool().unwrap_or(false) })
    }
}

/// One window to reopen.
#[derive(Clone, Debug, PartialEq)]
pub struct SetEntry {
    pub root: PathBuf,
    pub slot: u32,
}

pub fn load_set(path: &Path) -> Vec<SetEntry> {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Vec::new();
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some(SetEntry { root: PathBuf::from(e["root"].as_str()?), slot: e["slot"].as_u64()? as u32 }))
        .collect()
}

pub fn save_set(path: &Path, set: &[SetEntry]) {
    let arr: Vec<Value> = set.iter().map(|e| json!({"root": e.root.to_string_lossy(), "slot": e.slot})).collect();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, Value::Array(arr).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// A window opened. `fresh`: the first window of a new run that is not reopening the last one
/// (`lumen-text some/folder`): the set starts over with it.
pub fn set_opened(set: &mut Vec<SetEntry>, me: SetEntry, fresh: bool) {
    if fresh {
        set.clear();
    }
    if !set.contains(&me) {
        set.push(me);
    }
}

/// A window closed; `keep` when it was the last one, "Quit" closed every window, or hot exit kept
/// unsaved tabs of it.
pub fn set_closed(set: &mut Vec<SetEntry>, me: &SetEntry, keep: bool) {
    if !keep {
        set.retain(|e| e != me);
    }
}

/// A window switched project (or slot).
pub fn set_moved(set: &mut Vec<SetEntry>, old: &SetEntry, new: SetEntry) {
    match set.iter().position(|e| e == old) {
        Some(i) => set[i] = new,
        None => set.push(new),
    }
    let mut seen = Vec::new();
    set.retain(|e| {
        let dup = seen.contains(e);
        seen.push(e.clone());
        !dup
    });
}

/// The windows to reopen: the first is this process, the others are started with these
/// arguments. Folders that no longer exist are skipped.
pub fn restore_plan(set: &[SetEntry]) -> Option<(SetEntry, Vec<Vec<String>>)> {
    let mut alive = set.iter().filter(|e| e.root.is_dir()).cloned();
    let first = alive.next()?;
    let rest =
        alive.map(|e| vec!["--slot".to_string(), e.slot.to_string(), e.root.to_string_lossy().into_owned()]).collect();
    Some((first, rest))
}

/// State home of the editor (`~/.local/state/lumen-text`).
pub fn state_base() -> Option<PathBuf> {
    crate::paths::state_home().map(|b| b.join("lumen-text"))
}

pub fn set_path() -> Option<PathBuf> {
    state_base().map(|b| b.join("windows.json"))
}

/// Session file name of `root`'s window `slot`.
pub fn session_file(base: &Path, root: &Path, slot: u32) -> PathBuf {
    let h =
        root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    if slot == 0 {
        base.join(format!("session-{h:016x}.json"))
    } else {
        base.join(format!("session-{h:016x}-{slot}.json"))
    }
}

/// Start another window process with `args`.
#[cfg_attr(test, allow(dead_code))]
pub fn spawn_window(args: &[String]) -> std::io::Result<()> {
    // Inside an AppImage the binary lives in a mount that ends with this process: start the
    // AppImage itself.
    let appimage = std::env::var_os("APPIMAGE").filter(|a| !a.is_empty()).map(PathBuf::from);
    let exe = match &appimage {
        Some(a) => a.clone(),
        None => std::env::current_exe()?,
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if appimage.is_some() {
        for k in ["APPDIR", "ARGV0", "OWD", "LUMEN_ASSETS"] {
            cmd.env_remove(k);
        }
    }
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        // Its own session: closing the terminal this window came from does not close it.
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub struct WinState {
    /// Which of its project's windows this is (picks the session file).
    pub slot: u32,
    /// Restore the session when no file was given (see [`Launch`]).
    pub restore: bool,
    pub inst: Option<Instance>,
    /// Geometry for the window about to be created (from the session or a moved tab), and the
    /// last known one where there is no window (headless).
    pub geom: Option<Geom>,
    /// Closing because "Quit" closes every window: stay in the set reopened next time.
    pub quitting_all: bool,
    /// Tests: sessions, the window set and handoff files go here instead of the state home.
    pub base: Option<PathBuf>,
    /// Tests: windows "started" (their arguments) instead of real processes.
    #[cfg(test)]
    pub spawned: Vec<Vec<String>>,
}

impl Default for WinState {
    fn default() -> Self {
        let l = LAUNCH.get().copied().unwrap_or(Launch { slot: 0, restore: true });
        WinState {
            slot: l.slot,
            restore: l.restore,
            inst: None,
            geom: None,
            quitting_all: false,
            base: None,
            #[cfg(test)]
            spawned: Vec::new(),
        }
    }
}

impl WinState {
    pub fn base(&self) -> Option<PathBuf> {
        self.base.clone().or_else(state_base)
    }

    fn set_file(&self) -> Option<PathBuf> {
        if self.base.is_some() { self.base().map(|b| b.join("windows.json")) } else { set_path() }
    }

    fn handoff_dir(&self) -> Option<PathBuf> {
        self.base().map(|b| b.join("handoff"))
    }

    fn me(&self) -> Option<SetEntry> {
        self.inst.as_ref().map(|i| SetEntry { root: i.root.clone(), slot: i.slot })
    }

    fn edit_set(&self, f: impl FnOnce(&mut Vec<SetEntry>)) {
        let Some(path) = self.set_file() else { return };
        let mut set = load_set(&path);
        let before = set.clone();
        f(&mut set);
        if set != before {
            save_set(&path, &set);
        }
    }
}

impl App {
    /// Session file of this window (project root + slot).
    pub fn win_session_path(&self) -> Option<PathBuf> {
        Some(session_file(&self.win.base()?, &self.root, self.win.slot))
    }

    /// Backups (hot exit, crash recovery) of this window: one folder per project and slot, so
    /// two windows of one project never sweep or reopen each other's unsaved buffers.
    pub fn backup_dir(&self) -> Option<PathBuf> {
        let d = crate::backup::project_dir(&self.root)?;
        if self.win.slot == 0 {
            return Some(d);
        }
        let key = d.file_name()?.to_string_lossy().into_owned();
        Some(d.with_file_name(format!("{key}-{}", self.win.slot)))
    }

    /// Current geometry: from the window when there is one.
    pub fn win_geom_now(&self) -> Option<Geom> {
        let Some(g) = &self.gfx else { return self.win.geom };
        let s = g.window.inner_size();
        let k = g.window.scale_factor().max(0.1);
        let pos = g.window.outer_position().ok().map(|p| (p.x, p.y));
        Some(Geom {
            pos,
            w: (s.width as f64 / k) as u32,
            h: (s.height as f64 / k) as u32,
            max: g.window.is_maximized(),
        })
    }

    /// Session hooks (see `save_session` / `restore_session`).
    pub fn win_session_save(&self, v: &mut Value) {
        if let Some(g) = self.win_geom_now() {
            v["window"] = g.to_json();
        }
    }

    pub fn win_session_restore(&mut self, v: &Value) {
        if let Some(g) = Geom::from_json(&v["window"]) {
            self.win.geom = Some(g);
        }
    }

    /// Size, position and maximized state for the window being created. A saved position that
    /// is on no monitor any more (a screen was unplugged) is dropped.
    pub fn win_attrs(
        &self,
        el: &winit::event_loop::ActiveEventLoop,
        attrs: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        let Some(g) = self.win.geom else { return attrs };
        let mut attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(g.w as f64, g.h as f64));
        if let Some((x, y)) = g.pos {
            let on_screen = el.available_monitors().any(|m| {
                let (p, s) = (m.position(), m.size());
                x >= p.x - 64 && y >= p.y - 64 && x < p.x + s.width as i32 - 64 && y < p.y + s.height as i32 - 64
            });
            if on_screen {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
        attrs.with_maximized(g.max)
    }

    /// Become a handoff target and join the window set. `fresh`: a run that does not reopen the
    /// last set of windows (see [`set_opened`]).
    pub fn win_attach(&mut self, inst: Option<Instance>, fresh: bool) {
        let Some(mut inst) = inst else { return };
        inst.root = self.root.clone();
        inst.slot = self.win.slot;
        inst.focus = instance::now_ms();
        inst.publish();
        let alone = inst.others.is_empty();
        self.win.inst = Some(inst);
        if let Some(me) = self.win.me() {
            self.win.edit_set(|s| set_opened(s, me, fresh && alone));
        }
    }

    /// The window got the focus: it is now where files without a project go.
    pub fn win_focused(&mut self) {
        if let Some(i) = self.win.inst.as_mut() {
            i.focus = instance::now_ms();
            i.publish();
        }
    }

    /// The project root changed (`switch_project_now`): take a free slot of the new project.
    pub fn win_root_changed(&mut self) {
        let Some(inst) = self.win.inst.as_mut() else { return };
        let old = SetEntry { root: inst.root.clone(), slot: inst.slot };
        let others = inst.others_live();
        inst.root = self.root.clone();
        inst.slot = instance::free_slot(&others, &self.root);
        inst.publish();
        self.win.slot = inst.slot;
        let new = SetEntry { root: inst.root.clone(), slot: inst.slot };
        self.win.edit_set(|s| set_moved(s, &old, new));
    }

    /// The window is closing for good (`try_quit` said yes).
    pub fn win_close(&mut self) {
        let Some(inst) = self.win.inst.take() else { return };
        inst.leave();
        let me = SetEntry { root: inst.root.clone(), slot: inst.slot };
        // Unsaved tabs kept by hot exit come back with their window: keep it in the set too.
        let kept_unsaved = self.config.hot_exit && self.docs.iter().any(|d| d.buf.is_dirty());
        let keep = self.win.quitting_all || kept_unsaved || inst.others_live().is_empty();
        self.win.edit_set(|s| set_closed(s, &me, keep));
    }

    /// "Quit": close this window and ask every other one to close too (each asks about its own
    /// unsaved tabs). All of them are reopened next time. True when this window may exit.
    pub fn quit_all(&mut self) -> bool {
        let others = self.win.inst.as_ref().map(|i| (i.dir.clone(), i.others_live()));
        self.win.quitting_all = true;
        if !self.try_quit() {
            self.win.quitting_all = false;
            return false;
        }
        for (dir, wins) in others.iter() {
            for w in wins {
                let _ = instance::send(dir, &w.id, &Msg::Quit);
            }
        }
        true
    }

    /// Another window's "Quit" reached this one.
    pub fn win_quit_requested(&mut self) {
        self.win.quitting_all = true;
        if self.try_quit() {
            std::process::exit(0);
        }
        self.win.quitting_all = false;
        if let Some(g) = &self.gfx {
            g.window.focus_window();
        }
    }

    #[cfg(not(test))]
    fn win_spawn(&mut self, args: Vec<String>) -> std::io::Result<()> {
        spawn_window(&args)
    }

    /// Tests record the windows they would start.
    #[cfg(test)]
    fn win_spawn(&mut self, args: Vec<String>) -> std::io::Result<()> {
        self.win.spawned.push(args);
        Ok(())
    }

    /// "File: New window": another window on this project, without tabs.
    pub fn new_window(&mut self) {
        let args = vec!["--new-window".to_string(), self.root.to_string_lossy().into_owned()];
        if let Err(e) = self.win_spawn(args) {
            self.say(format!("could not open a new window: {e}"));
        }
    }

    /// Open a folder in a window of its own (the window it is open in, if any, comes forward).
    pub fn open_folder_in_new_window(&mut self, dir: PathBuf) {
        let dir = crate::paths::canonical(&dir).unwrap_or(dir);
        if !dir.is_dir() {
            return self.say(format!("{} is not a folder", dir.display()));
        }
        if dir == self.root {
            return self.say("that project is open in this window");
        }
        // A plain invocation: focuses the window that has it open, or opens a new one.
        match self.win_spawn(vec![dir.to_string_lossy().into_owned()]) {
            Ok(()) => self.say(format!("opening {} in a new window", crate::app_topbar::project_name(&dir))),
            Err(e) => self.say(format!("could not open a new window: {e}")),
        }
    }

    /// The window (other than this one) that has `dir` open as its project.
    pub fn win_focus_existing(&mut self, dir: &Path) -> bool {
        let Some(inst) = self.win.inst.as_ref() else { return false };
        let Some(w) = inst.others_live().into_iter().filter(|w| w.root == dir).max_by_key(|w| w.focus) else {
            return false;
        };
        instance::send(&inst.dir, &w.id, &Msg::Open(Vec::new())).is_ok()
    }

    /// The handoff record of tab `idx`: path, caret, scroll and the text when it is unsaved.
    pub fn tab_handoff(&self, idx: usize, geom: Option<Geom>) -> Option<Value> {
        let d = self.docs.get(idx)?;
        let (line, col) = d.buf.line_col(d.ed.head());
        let dirty = d.buf.is_dirty();
        let mut v = json!({"name": d.name, "line": line, "col": col, "top": d.target, "dirty": dirty});
        if let Some(p) = d.path() {
            v["path"] = json!(p.to_string_lossy());
        }
        // An image tab has no text: the receiving window reopens the file itself.
        if d.img.is_none() {
            v["encoding"] = json!(d.buf.encoding.id());
            if dirty || d.path().is_none() {
                v["text"] = json!(d.buf.to_string());
            }
        }
        if let Some(g) = geom {
            v["window"] = g.to_json();
        }
        Some(v)
    }

    /// Geometry of a window opened by dropping a tab at `drop` (pointer, logical px in this
    /// window): this window's size, its top-left corner near the pointer.
    fn drop_geom(&self, drop: Option<(f64, f64)>) -> Option<Geom> {
        let mut g = self.win_geom_now()?;
        g.max = false;
        g.pos = match (g.pos, drop, &self.gfx) {
            (Some((wx, wy)), Some((mx, my)), Some(gfx)) => {
                let k = gfx.window.scale_factor();
                Some((wx + (mx * k) as i32 - (60.0 * k) as i32, wy + (my * k) as i32 - (20.0 * k) as i32))
            }
            _ => None,
        };
        Some(g)
    }

    /// Move tab `idx` to a new window (its unsaved text included). `drop`: where the tab was
    /// dropped outside this window, if it was dragged.
    pub fn move_tab_to_new_window(&mut self, idx: usize, drop: Option<(f64, f64)>) {
        let geom = self.drop_geom(drop);
        let Some(v) = self.tab_handoff(idx, geom) else { return };
        let name = self.docs[idx].name.clone();
        let Some(dir) = self.win.handoff_dir() else { return self.say("no state folder to hand the tab over") };
        let file = dir.join(format!("{}-{}.json", std::process::id(), instance::now_ms()));
        let written = std::fs::create_dir_all(&dir).and_then(|_| {
            let mut f = std::fs::File::create(&file)?;
            f.write_all(v.to_string().as_bytes())?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&file);
            return self.say(format!("could not move {name}: {e}"));
        }
        let args = vec![
            "--new-window".to_string(),
            "--take".to_string(),
            file.to_string_lossy().into_owned(),
            self.root.to_string_lossy().into_owned(),
        ];
        match self.win_spawn(args) {
            Ok(()) => {
                self.close_tab(idx, true);
                self.say(format!("{name} moved to a new window"));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&file);
                self.say(format!("could not open a new window: {e}"));
            }
        }
    }

    /// Open a tab handed over by another window (see [`move_tab_to_new_window`]); the file is
    /// deleted once the tab is open. Returns the tab.
    pub fn take_handoff(&mut self, file: &Path) -> Option<usize> {
        // Claim it first: a window recovering orphans must not open it as well.
        let claimed = file.with_extension(format!("taken-{}", std::process::id()));
        std::fs::rename(file, &claimed).ok()?;
        let v = std::fs::read_to_string(&claimed).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let idx = v.as_ref().and_then(|v| self.open_handoff(v));
        if idx.is_some() || v.is_none() {
            let _ = std::fs::remove_file(&claimed);
        } else {
            // Could not open it: put it back for a later window.
            let _ = std::fs::rename(&claimed, file);
        }
        idx
    }

    fn open_handoff(&mut self, v: &Value) -> Option<usize> {
        if let Some(g) = Geom::from_json(&v["window"]) {
            self.win.geom = Some(g);
        }
        let text = v["text"].as_str();
        let idx = match v["path"].as_str().map(PathBuf::from) {
            Some(p) if p.exists() => self.open_path(&p, None)?,
            path => {
                // Untitled, or a file deleted since: an unsaved document with its text.
                text?;
                let buf = crate::buffer::Buffer::from_rope(ropey::Rope::new(), path);
                let lang = buf.path.as_ref().and_then(|p| crate::highlight::language_for(&p.to_string_lossy()));
                let hl = lang.and_then(|l| self.tokenizer_for(l)).map(crate::highlight::Highlighter::new);
                let mut d = crate::doc::Doc::new(buf, hl, lang, "//", self.tx.cell_w);
                if let Some(n) = v["name"].as_str() {
                    d.name = n.to_string();
                }
                self.docs.push(d);
                self.docs.len() - 1
            }
        };
        let d = &mut self.docs[idx];
        if let Some(t) = text {
            d.replace_text(t);
        }
        // Encoding and BOM travel with the text: saving the moved tab must not change the file's bytes.
        if let Some(enc) = v["encoding"].as_str().and_then(crate::encoding::TextEncoding::from_id) {
            if d.img.is_none() {
                d.buf.encoding = enc;
            }
        }
        let (line, col) = (v["line"].as_u64().unwrap_or(0) as usize, v["col"].as_u64().unwrap_or(0) as usize);
        let line = line.min(d.buf.line_count().saturating_sub(1));
        let p = d.buf.pos(line, col.min(d.buf.line_len(line)));
        d.ed.click(&mut d.buf, p, false);
        let top = v["top"].as_f64().unwrap_or(0.0);
        d.top = top;
        d.target = top;
        d.reveal = false;
        self.activate(idx);
        Some(idx)
    }

    /// Reopen tabs whose move to a new window did not complete (older handoff files).
    pub fn take_orphans(&mut self) {
        let Some(dir) = self.win.handoff_dir() else { return };
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let old: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| {
                e.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .is_some_and(|t| t.elapsed().unwrap_or_default() > ORPHAN_AGE)
            })
            .map(|e| e.path())
            .collect();
        let n = old.iter().filter(|f| self.take_handoff(f).is_some()).count();
        if n > 0 {
            self.say_for(format!("reopened {n} tab(s) left over from moving a tab to a new window"), 6);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit;

    fn e(root: &str, slot: u32) -> SetEntry {
        SetEntry { root: PathBuf::from(root), slot }
    }

    #[test]
    fn the_window_set_follows_opens_closes_and_quit() {
        let mut s = vec![e("/old", 0)];
        // A fresh run (`lumen-text /a`) starts the set over.
        set_opened(&mut s, e("/a", 0), true);
        set_opened(&mut s, e("/a", 1), false);
        set_opened(&mut s, e("/b", 0), false);
        set_opened(&mut s, e("/b", 0), false);
        assert_eq!(s, [e("/a", 0), e("/a", 1), e("/b", 0)]);
        // Closing one window while others stay open drops it.
        set_closed(&mut s, &e("/a", 1), false);
        assert_eq!(s, [e("/a", 0), e("/b", 0)]);
        // "Quit" (or the last window) keeps everything.
        set_closed(&mut s, &e("/a", 0), true);
        set_closed(&mut s, &e("/b", 0), true);
        assert_eq!(s, [e("/a", 0), e("/b", 0)]);
        // Switching project replaces the entry in place.
        set_moved(&mut s, &e("/b", 0), e("/c", 0));
        assert_eq!(s, [e("/a", 0), e("/c", 0)]);
        set_moved(&mut s, &e("/c", 0), e("/a", 0));
        assert_eq!(s, [e("/a", 0)], "no duplicate entries");
    }

    #[test]
    fn the_restore_plan_reopens_every_window_of_the_set() {
        let d = testkit::tmp("winplan");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let set = vec![
            SetEntry { root: a.clone(), slot: 0 },
            SetEntry { root: d.join("gone"), slot: 0 },
            SetEntry { root: a.clone(), slot: 1 },
            SetEntry { root: b.clone(), slot: 0 },
        ];
        let f = d.join("windows.json");
        save_set(&f, &set);
        assert_eq!(load_set(&f), set);
        let (first, rest) = restore_plan(&load_set(&f)).unwrap();
        assert_eq!(first, SetEntry { root: a.clone(), slot: 0 });
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(rest, vec![vec!["--slot".into(), "1".into(), s(&a)], vec!["--slot".into(), "0".into(), s(&b)]]);
        // Every planned window parses back into a window that takes its slot.
        let cli = crate::cli::parse(rest[0].clone());
        assert_eq!((cli.win.slot, cli.args), (Some(1), vec![s(&a)]));
        assert!(restore_plan(&[]).is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn geometry_round_trips() {
        let g = Geom { pos: Some((-1200, 40)), w: 1400, h: 900, max: false };
        assert_eq!(Geom::from_json(&g.to_json()), Some(g));
        let wl = Geom { pos: None, w: 800, h: 600, max: true };
        assert_eq!(Geom::from_json(&wl.to_json()), Some(wl));
        assert_eq!(Geom::from_json(&json!({"w": 5, "h": 99999})).map(|g| (g.w, g.h)), Some((320, 16_000)));
        assert_eq!(Geom::from_json(&Value::Null), None);
    }

    /// Two windows on one project and one on another: each keeps its tabs and geometry.
    #[test]
    fn session_round_trip_with_several_windows() {
        let _g = testkit::lock();
        let dir = testkit::tmp("winsess");
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let state = dir.join("state");
        let files: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"].iter().map(|n| dir.join(n)).collect();
        for f in &files {
            std::fs::write(f, "one\ntwo\nthree\nfour\n").unwrap();
        }
        let o = other.join("o.txt");
        std::fs::write(&o, "x\n").unwrap();
        // (root, slot, files, caret line, geometry)
        let windows = [
            (
                dir.clone(),
                0u32,
                vec![files[0].clone()],
                1usize,
                Geom { pos: Some((10, 20)), w: 900, h: 700, max: false },
            ),
            (
                dir.clone(),
                1,
                vec![files[1].clone(), files[2].clone()],
                3,
                Geom { pos: Some((900, 20)), w: 800, h: 600, max: false },
            ),
            (other.clone(), 0, vec![o.clone()], 0, Geom { pos: None, w: 1200, h: 800, max: true }),
        ];
        for (root, slot, open, line, geom) in &windows {
            let mut app = testkit::test_app(&dir);
            app.win.base = Some(state.clone());
            while !app.docs.is_empty() {
                app.close_tab(0, true);
            }
            app.root = root.clone();
            app.win.slot = *slot;
            app.win.geom = Some(*geom);
            for f in open {
                app.open_path(f, Some((*line, 1)));
            }
            app.save_session();
        }
        assert!(session_file(&state, &dir, 1).exists() && session_file(&state, &dir, 0).exists());
        for (root, slot, open, line, geom) in &windows {
            let mut app = testkit::test_app(&dir);
            app.win.base = Some(state.clone());
            while !app.docs.is_empty() {
                app.close_tab(0, true);
            }
            app.root = root.clone();
            app.win.slot = *slot;
            app.win.geom = None;
            app.restore_session();
            let got: Vec<PathBuf> = app.docs.iter().filter_map(|d| d.path().map(Path::to_path_buf)).collect();
            assert_eq!(&got, open, "tabs of window {slot} of {}", root.display());
            let d = &app.docs[app.active];
            assert_eq!(d.buf.line_col(d.ed.head()).0, *line);
            assert_eq!(app.win.geom, Some(*geom), "geometry of window {slot}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_moved_tab_carries_its_unsaved_text_caret_and_scroll() {
        let _g = testkit::lock();
        let dir = testkit::tmp("winmove");
        let state = dir.join("state");
        let mut app = testkit::test_app(&dir);
        app.win.base = Some(state.clone());
        let f = dir.join("m.txt");
        std::fs::write(&f, (0..50).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        let i = app.open_path(&f, Some((20, 2))).unwrap();
        {
            let d = &mut app.docs[i];
            d.ed.insert_text(&mut d.buf, "EDIT ");
            d.sync_edits();
            d.target = 12.0;
        }
        let unsaved = app.docs[i].buf.to_string();
        let caret = app.docs[i].buf.line_col(app.docs[i].ed.head());
        let u = testkit::untitled(&mut app, "scratch notes");
        let n = app.docs.len();

        app.move_tab_to_new_window(i, None);
        app.move_tab_to_new_window(u - 1, None);
        assert_eq!(app.docs.len(), n - 2, "both tabs left this window");
        assert_eq!(app.win.spawned.len(), 2);
        let args = app.win.spawned[0].clone();
        let cli = crate::cli::parse(args);
        assert!(cli.win.new_window);
        assert_eq!(cli.args, [dir.to_string_lossy().into_owned()], "same project");
        let take = cli.win.take.expect("handoff file");
        assert!(take.exists(), "the text is on disk until the new window takes it");
        assert!(!std::fs::read_to_string(&f).unwrap().contains("EDIT"), "nothing was saved");
        let take2 = crate::cli::parse(app.win.spawned[1].clone()).win.take.unwrap();
        drop(app);

        // The new window.
        let mut win2 = testkit::test_app(&dir);
        win2.win.base = Some(state.clone());
        let j = win2.take_handoff(&take).expect("tab opened");
        let d = &win2.docs[j];
        assert_eq!(d.path(), Some(f.as_path()));
        assert!(d.buf.is_dirty(), "still unsaved");
        assert_eq!(d.buf.to_string(), unsaved);
        assert_eq!(d.buf.line_col(d.ed.head()), caret);
        assert_eq!(d.target, 12.0);
        assert!(!take.exists(), "taken files are deleted");
        let k = win2.take_handoff(&take2).expect("untitled tab opened");
        assert_eq!(win2.docs[k].buf.to_string(), "scratch notes");
        assert!(win2.docs[k].path().is_none() && win2.docs[k].buf.is_dirty());
        assert!(win2.take_handoff(&take).is_none(), "a file is taken once");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn orphaned_handoffs_are_reopened() {
        let _g = testkit::lock();
        let dir = testkit::tmp("winorphan");
        let state = dir.join("state");
        let hd = state.join("handoff");
        std::fs::create_dir_all(&hd).unwrap();
        let old = hd.join("1-1.json");
        std::fs::write(&old, json!({"name": "lost.md", "text": "keep me", "line": 0, "col": 0}).to_string()).unwrap();
        let fresh = hd.join("2-2.json");
        std::fs::write(&fresh, json!({"name": "new.md", "text": "in flight", "line": 0, "col": 0}).to_string())
            .unwrap();
        let past = std::time::SystemTime::now() - Duration::from_secs(120);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(past).unwrap();
        let mut app = testkit::test_app(&dir);
        app.win.base = Some(state.clone());
        let n = app.docs.len();
        app.take_orphans();
        assert_eq!(app.docs.len(), n + 1, "only the old one");
        assert_eq!(app.docs[app.active].buf.to_string(), "keep me");
        assert!(!old.exists() && fresh.exists(), "a move in flight is left to its window");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_window_of_a_project_has_its_own_backups() {
        let _g = testkit::lock();
        let dir = testkit::tmp("winbak");
        let mut app = testkit::test_app(&dir);
        let first = app.backup_dir();
        assert_eq!(first, crate::backup::project_dir(&dir), "the first window keeps the old folder");
        app.win.slot = 2;
        let second = app.backup_dir().unwrap();
        assert_ne!(Some(second.clone()), first);
        assert_eq!(second.parent(), first.as_deref().and_then(Path::parent));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_window_and_open_folder_start_processes() {
        let _g = testkit::lock();
        let dir = testkit::tmp("winnew");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut app = testkit::test_app(&dir);
        app.run_cmd(crate::app_overlay::Cmd::NewWindow);
        assert_eq!(app.win.spawned, [vec!["--new-window".to_string(), dir.to_string_lossy().into_owned()]]);
        app.open_folder_in_new_window(sub.clone());
        assert_eq!(app.win.spawned[1], [sub.to_string_lossy().into_owned()], "routed like a second invocation");
        app.open_folder_in_new_window(dir.clone());
        assert_eq!(app.win.spawned.len(), 2, "this window's own project is not opened again");
        std::fs::remove_dir_all(&dir).ok();
    }
}
