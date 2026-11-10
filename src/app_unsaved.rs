//! "Unsaved changes" question: Salvar tudo / Descartar / Cancelar.
//!
//! Asked before an action that would throw unsaved tabs away ("Fechar outras" / "Fechar todas",
//! switching project; "Fechar todas" also lists unsaved *pinned* tabs, which stay open but can be saved) and, for a project switch, also when a debug session is running (it is
//! stopped as part of the action). The card rides on [`Overlay::Ctx`] like the delete
//! confirmation does ([`CtxMenu::unsaved`]), so key/mouse/hover routing and the drawing hooks
//! are the context menu's own; everything specific lives here.

use crate::{
    app::App,
    app_ctxmenu::{CtxMenu, Target},
    app_overlay::{Overlay, Prompt, PromptKind},
    field::TextField,
    ui::{Canvas, Rect},
};
use std::path::{Path, PathBuf};
use winit::keyboard::{Key, NamedKey};

/// What to do once the user has answered.
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    /// "Fechar outras" (`keep = Some(tab)`) or "Fechar todas" (`None`).
    CloseTabs { keep: Option<usize> },
    /// Open another project folder.
    SwitchProject(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Save,
    Discard,
    Cancel,
}

pub struct Unsaved {
    pub pending: Pending,
    /// Names of the unsaved tabs involved.
    pub names: Vec<String>,
    /// How many of them are pinned tabs that stay open ("Fechar todas"): only saved, never closed.
    pub kept: usize,
    /// A debug session is running and will be stopped (project switch).
    pub debug: bool,
    /// Focused button.
    pub sel: Choice,
    /// Buttons as drawn last frame, indexed like [`Choice`]: Save, Discard, Cancel.
    pub rects: [Rect; 3],
}

impl Unsaved {
    pub fn new(pending: Pending, names: Vec<String>, debug: bool) -> Self {
        let sel = if names.is_empty() { Choice::Discard } else { Choice::Save };
        Unsaved { pending, names, kept: 0, debug, sel, rects: [Rect::default(); 3] }
    }

    pub fn with_kept(mut self, kept: usize) -> Self {
        self.kept = kept.min(self.names.len());
        self
    }

    /// "Salvar tudo" only makes sense when there is something to save.
    pub fn has_save(&self) -> bool {
        !self.names.is_empty()
    }

    /// Buttons that are shown, left to right.
    pub fn choices(&self) -> Vec<Choice> {
        let mut v = vec![Choice::Cancel, Choice::Discard];
        if self.has_save() {
            v.push(Choice::Save);
        }
        v
    }

    pub fn label(&self, c: Choice) -> &'static str {
        match c {
            Choice::Save => "Salvar tudo",
            Choice::Discard if self.has_save() => "Descartar",
            Choice::Discard => "Parar e trocar",
            Choice::Cancel => "Cancelar",
        }
    }

    /// Move the focus along the visible buttons (wraps).
    pub fn step(&mut self, dir: i32) {
        let v = self.choices();
        let i = v.iter().position(|c| *c == self.sel).unwrap_or(0) as i32;
        self.sel = v[(i + dir).rem_euclid(v.len() as i32) as usize];
    }

    /// Card text: (title, body lines).
    pub fn text(&self) -> (String, Vec<String>) {
        let n = self.names.len();
        let title = match (n, self.debug) {
            (0, _) => "Há uma depuração em andamento".to_string(),
            (1, _) => format!("“{}” tem alterações não salvas", self.names[0]),
            _ => format!("{n} abas têm alterações não salvas"),
        };
        let mut body = Vec::new();
        if n > 1 {
            let shown: Vec<&str> = self.names.iter().take(3).map(String::as_str).collect();
            let more = if n > 3 { format!(" e mais {}", n - 3) } else { String::new() };
            body.push(format!("{}{more}", shown.join(", ")));
        }
        if self.debug {
            body.push(if n == 0 { "Ela será encerrada ao trocar de projeto." } else { "A depuração em andamento também será encerrada." }.into());
        }
        if matches!(self.pending, Pending::SwitchProject(_)) {
            body.push("As abas abertas serão fechadas ao trocar de projeto.".into());
        } else if self.kept > 0 && self.kept == n {
            body.push(if n == 1 { "A aba fixada continua aberta." } else { "As abas fixadas continuam abertas." }.into());
        } else if self.kept > 0 {
            body.push(format!("{} fixada(s) continua(m) aberta(s); as demais serão fechadas.", self.kept));
        } else if n > 0 {
            body.push("As abas selecionadas serão fechadas.".into());
        }
        (title, body)
    }

    /// Answer for a key press (letters work in Portuguese: s, d, c).
    pub fn key(&mut self, key: &Key) -> Option<Choice> {
        match key {
            Key::Named(NamedKey::Escape) => Some(Choice::Cancel),
            Key::Named(NamedKey::ArrowLeft) => {
                self.step(-1);
                None
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::Tab) => {
                self.step(1);
                None
            }
            Key::Named(NamedKey::Enter) => Some(self.sel),
            Key::Character(s) => match s.to_lowercase().as_str() {
                "s" if self.has_save() => Some(Choice::Save),
                "d" => Some(Choice::Discard),
                "c" | "n" => Some(Choice::Cancel),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Tabs that "Fechar outras" (`keep = Some(i)`) / "Fechar todas" (`None`) close, highest index
/// first, plus the unsaved ones among them. Pinned tabs always stay.
pub fn tabs_to_close(pinned: &[bool], dirty: &[bool], keep: Option<usize>) -> (Vec<usize>, Vec<usize>) {
    let all: Vec<usize> = (0..pinned.len()).rev().filter(|&i| Some(i) != keep && !pinned[i]).collect();
    let unsaved = all.iter().copied().filter(|&i| dirty[i]).collect();
    (all, unsaved)
}

/// Unsaved tabs the question lists and "Salvar tudo" saves, in tab order, and how many of them are
/// pinned tabs that stay open. "Fechar todas" (`keep = None`) includes unsaved *pinned* tabs (they
/// are not closed, but the user is asked about them and may save them); "Fechar outras" only lists
/// what it closes.
pub fn tabs_to_ask(pinned: &[bool], dirty: &[bool], keep: Option<usize>) -> (Vec<usize>, usize) {
    let mut v: Vec<usize> = (0..pinned.len()).filter(|&i| dirty[i] && Some(i) != keep && (!pinned[i] || keep.is_none())).collect();
    v.sort_unstable();
    let kept = v.iter().filter(|&&i| pinned[i]).count();
    (v, kept)
}

impl App {
    /// Show the card for `pending`.
    pub fn ask_unsaved(&mut self, pending: Pending, names: Vec<String>, debug: bool) {
        self.ask_unsaved_kept(pending, names, 0, debug);
    }

    fn ask_unsaved_kept(&mut self, pending: Pending, names: Vec<String>, kept: usize, debug: bool) {
        self.hover = None;
        self.hover_rect = None;
        self.overlay = Overlay::Ctx(Box::new(CtxMenu {
            target: Target::Root(self.root.clone()),
            entries: Vec::new(),
            at: (0, 0),
            sel: None,
            rects: Vec::new(),
            card: Rect::default(),
            confirm: None,
            unsaved: Some(Unsaved::new(pending, names, debug).with_kept(kept)),
        }));
        self.request_redraw();
    }

    fn tab_flags(&self) -> (Vec<bool>, Vec<bool>) {
        (self.docs.iter().map(|d| d.pinned).collect(), self.docs.iter().map(|d| d.buf.is_dirty()).collect())
    }

    /// "Fechar outras" / "Fechar todas": closes at once when nothing would be lost, else asks.
    pub fn close_tabs_request(&mut self, keep: Option<usize>) {
        let (pinned, dirty) = self.tab_flags();
        let (list, kept) = tabs_to_ask(&pinned, &dirty, keep);
        if list.is_empty() {
            return self.close_tabs_now(keep);
        }
        let mut names: Vec<String> = list.iter().filter_map(|&i| self.docs.get(i)).map(|d| d.name.clone()).collect();
        names.dedup();
        self.ask_unsaved_kept(Pending::CloseTabs { keep }, names, kept, false);
    }

    /// Close every unpinned tab but `keep`, discarding unsaved changes.
    pub fn close_tabs_now(&mut self, keep: Option<usize>) {
        let (pinned, dirty) = self.tab_flags();
        let (list, _) = tabs_to_close(&pinned, &dirty, keep);
        let kept_idx = keep.map(|k| k - list.iter().filter(|&&i| i < k).count());
        for i in list {
            self.close_tab(i, true);
        }
        if let Some(k) = kept_idx.filter(|&k| k < self.docs.len()) {
            // "Fechar outras": the tab the menu was opened on is the one left in front.
            self.activate(k);
        }
    }

    /// Save document `i` the way Ctrl+S does: format-on-save first (external formatter), then the
    /// atomic write and the follow-ups (LSP `didSave`, git gutter, disk stamp).
    pub fn save_doc(&mut self, i: usize) -> Result<(), String> {
        if self.format_on_save {
            self.format_external_doc(i, true);
        }
        let Some(d) = self.docs.get_mut(i) else { return Ok(()) };
        let name = d.name.clone();
        match d.buf.save() {
            Ok(()) => {
                d.git_stale = true;
                d.disk_mtime = d.stat_mtime();
                d.conflict = false;
                self.lsp_did_save(i);
                Ok(())
            }
            Err(e) => Err(format!("não foi possível salvar {name}: {e}")),
        }
    }

    /// Save these documents; stops at the first failure and reports it. A document without a file
    /// path cannot be written: it opens the "Salvar como" prompt, and the rest of the list (and
    /// `then`, the action that was waiting for the save) continue once that is answered.
    pub fn save_docs_then(&mut self, idxs: Vec<usize>, then: Option<Pending>) {
        let mut rest = idxs.into_iter();
        while let Some(i) = rest.next() {
            let Some(d) = self.docs.get(i) else { continue };
            if !d.buf.is_dirty() {
                continue;
            }
            if d.path().is_none() {
                return self.ask_save_as(i, rest.collect(), then, String::new());
            }
            if let Err(e) = self.save_doc(i) {
                self.refresh_git_status();
                return self.say(e);
            }
        }
        self.refresh_git_status();
        if let Some(p) = then {
            self.run_pending(p);
        }
    }

    /// "Salvar como" prompt for the untitled document `i`; `rest` and `then` continue afterwards.
    pub fn ask_save_as(&mut self, i: usize, rest: Vec<usize>, then: Option<Pending>, text: String) {
        let name = self.docs.get(i).map(|d| d.name.clone()).unwrap_or_default();
        self.overlay = Overlay::Prompt(Prompt {
            kind: PromptKind::SaveAs { doc: i, rest, then },
            title: format!("Salvar “{name}” como (caminho; relativo à raiz do projeto)"),
            field: TextField::with(&text),
        });
        self.request_redraw();
    }

    /// Where a "Salvar como" answer points: `~/x`, an absolute path, or relative to the project root.
    pub fn resolve_save_path(&self, text: &str) -> Option<PathBuf> {
        let t = text.trim();
        if t.is_empty() || t.ends_with('/') {
            return None;
        }
        Some(match t.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
            None if t.starts_with('/') => PathBuf::from(t),
            None => self.root.join(t),
        })
    }

    /// The "Salvar como" prompt was accepted with `text`.
    pub fn save_as_answer(&mut self, i: usize, rest: Vec<usize>, then: Option<Pending>, text: String) {
        let Some(path) = self.resolve_save_path(&text) else {
            return self.say("salvar cancelado: caminho vazio");
        };
        if path.is_dir() {
            self.say(format!("{} é uma pasta", path.display()));
            return self.ask_save_as(i, rest, then, text);
        }
        if path.exists() {
            self.say(format!("já existe: {}", path.display()));
            return self.ask_save_as(i, rest, then, text);
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return self.say(format!("não foi possível criar {}: {e}", parent.display()));
            }
        }
        self.set_doc_path(i, &path);
        if let Err(e) = self.save_doc(i) {
            return self.say(e);
        }
        self.refresh_tree();
        self.tree.reveal(&path);
        let name = self.docs.get(i).map(|d| d.name.clone()).unwrap_or_default();
        self.say(format!("{name} salvo"));
        self.save_docs_then(rest, then);
    }

    /// Give the untitled document `i` a file path: name, language, comment prefix, git base.
    pub fn set_doc_path(&mut self, i: usize, path: &Path) {
        let lang = crate::highlight::language_for(&path.to_string_lossy());
        let hl = lang.and_then(|l| self.tokenizer_for(l)).map(crate::highlight::Highlighter::new);
        let Some(d) = self.docs.get_mut(i) else { return };
        d.buf.path = Some(path.to_path_buf());
        d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        d.lang = lang;
        d.hl = hl;
        d.ed.comment = crate::highlight::comment_prefix(&path.to_string_lossy()).to_string();
        d.ed.jsx = matches!(lang, Some("tsx" | "jsx" | "html" | "vue" | "svelte"));
        d.sem.clear();
        d.lsp_open = false;
        d.git_base = crate::git::head_text(path);
        d.git_stale = true;
    }

    /// The user answered the card.
    pub fn unsaved_answer(&mut self, pending: Pending, choice: Choice) {
        self.overlay = Overlay::None;
        self.hover_rect = None;
        match choice {
            Choice::Cancel => self.say("cancelado"),
            Choice::Save => {
                let idxs: Vec<usize> = match &pending {
                    Pending::CloseTabs { keep } => {
                        let (pinned, dirty) = self.tab_flags();
                        tabs_to_ask(&pinned, &dirty, *keep).0
                    }
                    Pending::SwitchProject(_) => (0..self.docs.len()).collect(),
                };
                self.save_docs_then(idxs, Some(pending));
            }
            Choice::Discard => self.run_pending(pending),
        }
        self.request_redraw();
    }

    fn run_pending(&mut self, pending: Pending) {
        match pending {
            Pending::CloseTabs { keep } => self.close_tabs_now(keep),
            Pending::SwitchProject(path) => {
                if self.dbg.is_some() {
                    self.debug_stop();
                }
                self.switch_project_now(path);
            }
        }
    }

    /// Key press while the card is open (the caller already checked it is there).
    pub fn unsaved_key(&mut self, key: &Key) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        let Some(u) = m.unsaved.as_mut() else { return };
        if let Some(choice) = u.key(key) {
            let pending = u.pending.clone();
            self.unsaved_answer(pending, choice);
        }
        self.request_redraw();
    }

    /// Left click while the card is open: a button answers, a click outside cancels.
    pub fn unsaved_mouse(&mut self, x: f64, y: f64) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        let card = m.card;
        let Some(u) = m.unsaved.as_ref() else { return };
        let hit = u.choices().into_iter().find(|c| u.rects[*c as usize].contains(x, y));
        let choice = hit.or_else(|| (!card.contains(x, y)).then_some(Choice::Cancel));
        if let Some(c) = choice {
            let pending = u.pending.clone();
            self.unsaved_answer(pending, c);
        }
    }

    /// Pointer moved over the card: the hovered button takes the focus. True = redraw.
    pub fn unsaved_hover(&mut self, x: f64, y: f64) -> bool {
        let Overlay::Ctx(m) = &mut self.overlay else { return false };
        let Some(u) = m.unsaved.as_mut() else { return false };
        match u.choices().into_iter().find(|c| u.rects[*c as usize].contains(x, y)) {
            Some(c) if c != u.sel => {
                u.sel = c;
                true
            }
            _ => false,
        }
    }

    /// Draw the card (called from `draw_ctxmenu` with the menu taken out of the overlay).
    pub fn draw_unsaved(&mut self, cv: &mut Canvas, m: &mut CtxMenu) {
        let Some(u) = m.unsaved.as_mut() else { return };
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let tint = fg.with_alpha(0x1c);
        let sw = self.size.0 as i32;
        let (title, body) = u.text();
        let labels: Vec<(Choice, &'static str)> = u.choices().into_iter().map(|ch| (ch, u.label(ch))).collect();
        let widths: Vec<i32> = labels.iter().map(|(_, l)| (self.tx.ui_width(l, false) + 28).max(76)).collect();
        let buttons_w: i32 = widths.iter().sum::<i32>() + 8 * (widths.len() as i32 - 1);
        let w = (sw - 40).clamp(300, 480).max(buttons_w + 24).min((sw - 16).max(200));
        let h = 14 + 26 + body.len() as i32 * 20 + 14 + 30 + 12;
        let r = Rect::new((sw - w) / 2, 70, w, h);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        let t = fit_text(&mut self.tx, &title, r.w - 32, true);
        self.tx.draw_ui(cv, r.x + 16, r.y + 14, &t, fg, r, true);
        for (i, line) in body.iter().enumerate() {
            let l = fit_text(&mut self.tx, line, r.w - 32, false);
            self.tx.draw_ui(cv, r.x + 16, r.y + 40 + i as i32 * 20, &l, dim, r, false);
        }
        let bh = 30;
        let by = r.bottom() - bh - 12;
        let mut x = r.right() - 12 - buttons_w;
        for ((choice, label), bw) in labels.iter().zip(&widths) {
            let rect = Rect::new(x, by, *bw, bh);
            let focused = u.sel == *choice;
            cv.card(rect, 8, c.widget_bg, c.border);
            if focused {
                cv.fill_round(rect.inset(1), 7, tint);
            }
            let col = match choice {
                Choice::Discard => c.error,
                Choice::Save => c.accent_text,
                Choice::Cancel => fg,
            };
            let tw = self.tx.ui_width(label, false);
            self.tx.draw_ui(cv, rect.x + (rect.w - tw) / 2, rect.y + (bh - 18) / 2, label, col, rect, focused);
            u.rects[*choice as usize] = rect;
            self.layout.overlay_rows.push((rect, *choice as usize));
            x += bw + 8;
        }
        m.card = r;
    }
}

/// `s` shortened with an ellipsis to fit `max` px.
fn fit_text(tx: &mut crate::ui::TextCtx, s: &str, max: i32, bold: bool) -> String {
    if tx.ui_width(s, bold) <= max {
        return s.to_string();
    }
    let mut out: String = s.to_string();
    while out.chars().count() > 1 && tx.ui_width(&format!("{out}…"), bold) > max {
        out.pop();
    }
    format!("{out}…")
}

/// App fixtures for the tests of this module and of the explorer (no window, no LSP).
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;

    /// Held for the whole test: one `App` at a time (each owns a font system, a watcher and
    /// caches, and the memory test of `perf.rs` measures the whole test process); the freed heap
    /// is handed back to the OS when the test ends.
    pub struct Guard(std::sync::MutexGuard<'static, ()>);

    impl Drop for Guard {
        fn drop(&mut self) {
            crate::app_lsp::release_memory();
        }
    }

    pub fn lock() -> Guard {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        Guard(L.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// An `App` rooted at `dir` with `seed.txt` open (take [`lock`] first).
    pub fn test_app(dir: &Path) -> App {
        // Headless: no window, and the project is not added to the user's recent list.
        unsafe { std::env::set_var("LUMEN_HEADLESS", "1") };
        let assets = crate::config::assets_dir();
        let theme = crate::theme::Theme::load(&assets.join("themes").join("github-dark-default.json")).expect("theme");
        // Settings written by the code under test land here, not in the user's config.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.join(".config")) };
        let seed = dir.join("seed.txt");
        std::fs::write(&seed, "seed\n").unwrap();
        let wake: crate::lsp::Wake = std::sync::Arc::new(|| {});
        let mut app = App::new(vec![(seed, None)], dir.to_path_buf(), theme, wake, crate::config::Config::default());
        app.clip = crate::clipboard::Clipboard::in_process();
        app
    }

    /// Add a document without a path holding `text` (unsaved).
    pub fn untitled(app: &mut App, text: &str) -> usize {
        let mut d = crate::doc::Doc::new(crate::buffer::Buffer::from_str(""), None, None, "//", 8.0);
        d.replace_text(text);
        app.docs.push(d);
        app.docs.len() - 1
    }

    pub fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn close_lists_split_clean_from_unsaved() {
        let pinned = [false, true, false, false];
        let dirty = [false, false, true, false];
        // Close all: pinned (1) stays, highest index first; tab 2 is unsaved.
        assert_eq!(tabs_to_close(&pinned, &dirty, None), (vec![3, 2, 0], vec![2]));
        // Close others of tab 3: tab 3 and the pinned one stay.
        assert_eq!(tabs_to_close(&pinned, &dirty, Some(3)), (vec![2, 0], vec![2]));
        assert_eq!(tabs_to_close(&[false; 3], &[false; 3], Some(0)), (vec![2, 1], vec![]));
        // A pinned unsaved tab is not closed, so it is not asked about either.
        assert_eq!(tabs_to_close(&[true, false], &[true, false], None), (vec![1], vec![]));
    }

    #[test]
    fn card_text_and_buttons() {
        let u = Unsaved::new(Pending::CloseTabs { keep: None }, names(&["a.ts"]), false);
        assert_eq!(u.text().0, "“a.ts” tem alterações não salvas");
        assert_eq!(u.choices(), vec![Choice::Cancel, Choice::Discard, Choice::Save]);
        assert_eq!(u.sel, Choice::Save, "the safe answer is focused first");
        let u = Unsaved::new(Pending::SwitchProject("/x".into()), names(&["a", "b", "c", "d", "e"]), true);
        let (t, body) = u.text();
        assert_eq!(t, "5 abas têm alterações não salvas");
        assert!(body[0].contains("a, b, c e mais 2"), "{body:?}");
        assert!(body.iter().any(|l| l.contains("depuração")));
        // Debug only: nothing to save, so no "Salvar tudo" and Discard is the "stop" button.
        let u = Unsaved::new(Pending::SwitchProject("/x".into()), vec![], true);
        assert_eq!(u.choices(), vec![Choice::Cancel, Choice::Discard]);
        assert_eq!(u.label(Choice::Discard), "Parar e trocar");
        assert_eq!(u.text().0, "Há uma depuração em andamento");
    }

    #[test]
    fn close_all_also_asks_about_unsaved_pinned_tabs() {
        let pinned = [false, true, false, true];
        let dirty = [false, true, true, false];
        // Close all lists the unsaved pinned tab (1) too; it is only saved, never closed.
        assert_eq!(tabs_to_ask(&pinned, &dirty, None), (vec![1, 2], 1));
        // Close others of tab 2: only what it closes (the pinned one stays and is not asked about).
        assert_eq!(tabs_to_ask(&pinned, &dirty, Some(2)), (vec![], 0));
        assert_eq!(tabs_to_ask(&pinned, &dirty, Some(0)), (vec![2], 0));
        // Only a pinned tab is unsaved: Close all still has a question for it.
        assert_eq!(tabs_to_ask(&[true, false], &[true, false], None), (vec![0], 1));
        let u = Unsaved::new(Pending::CloseTabs { keep: None }, names(&["a.ts"]), false).with_kept(1);
        assert_eq!(u.text().1, ["A aba fixada continua aberta."]);
        let u = Unsaved::new(Pending::CloseTabs { keep: None }, names(&["a.ts", "b.ts"]), false).with_kept(1);
        assert!(u.text().1.iter().any(|l| l.contains("1 fixada(s) continua(m) aberta(s)")), "{:?}", u.text());
    }

    #[test]
    fn keys_pick_or_move_focus() {
        let mut u = Unsaved::new(Pending::CloseTabs { keep: Some(1) }, names(&["a"]), false);
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(u.key(&Key::Named(NamedKey::Escape)), Some(Choice::Cancel));
        assert_eq!(u.key(&ch("s")), Some(Choice::Save));
        assert_eq!(u.key(&ch("D")), Some(Choice::Discard));
        assert_eq!(u.key(&ch("x")), None);
        assert_eq!(u.key(&Key::Named(NamedKey::Enter)), Some(Choice::Save));
        // Arrows walk Cancelar | Descartar | Salvar tudo and wrap.
        assert_eq!(u.key(&Key::Named(NamedKey::ArrowRight)), None);
        assert_eq!(u.sel, Choice::Cancel, "wraps from the last button");
        u.step(1);
        assert_eq!(u.sel, Choice::Discard);
        assert_eq!(u.key(&Key::Named(NamedKey::Enter)), Some(Choice::Discard));
        // Without anything to save the "s" key does nothing.
        let mut u = Unsaved::new(Pending::SwitchProject("/x".into()), vec![], true);
        assert_eq!(u.key(&ch("s")), None);
        assert_eq!(u.sel, Choice::Discard);
    }

    use super::testkit::{lock, test_app, tmp, untitled};

    #[test]
    fn save_all_asks_for_a_path_for_untitled_tabs_then_continues() {
        let _g = lock();
        let dir = tmp("saveall");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("changed\n");
        let u = untitled(&mut app, "hello\n");
        assert!(app.docs[u].path().is_none() && app.docs[u].buf.is_dirty());
        app.close_tabs_request(None);
        assert!(matches!(&app.overlay, Overlay::Ctx(m) if m.unsaved.as_ref().is_some_and(|u| u.names.len() == 2)));
        app.unsaved_answer(Pending::CloseTabs { keep: None }, Choice::Save);
        // The file with a path was saved; the untitled one raised the "Salvar como" prompt and
        // nothing was closed yet.
        assert_eq!(std::fs::read_to_string(dir.join("seed.txt")).unwrap(), "changed\n");
        let Overlay::Prompt(p) = &app.overlay else { panic!("expected the Salvar como prompt") };
        assert!(matches!(&p.kind, PromptKind::SaveAs { doc, .. } if *doc == u));
        assert_eq!(app.docs.len(), 2);
        // Answer with a relative path (inside the project, in a folder that does not exist yet).
        let Overlay::Prompt(p) = std::mem::replace(&mut app.overlay, Overlay::None) else { unreachable!() };
        let PromptKind::SaveAs { doc, rest, then } = p.kind else { unreachable!() };
        app.save_as_answer(doc, rest, then, "sub/new.txt".into());
        assert_eq!(std::fs::read_to_string(dir.join("sub/new.txt")).unwrap(), "hello\n");
        assert!(app.docs.is_empty(), "the pending Close all ran after the save");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_refuses_existing_files_and_empty_names() {
        let _g = lock();
        let dir = tmp("saveas");
        let mut app = test_app(&dir);
        let u = untitled(&mut app, "x");
        app.save_as_answer(u, vec![], None, "seed.txt".into());
        assert_eq!(std::fs::read_to_string(dir.join("seed.txt")).unwrap(), "seed\n", "never overwrites");
        assert!(matches!(&app.overlay, Overlay::Prompt(p) if p.field.text == "seed.txt"), "asks again with the text kept");
        app.overlay = Overlay::None;
        app.save_as_answer(u, vec![], None, "  ".into());
        assert!(app.docs[u].path().is_none() && matches!(app.overlay, Overlay::None));
        assert_eq!(app.resolve_save_path("a/b.ts"), Some(app.root.join("a/b.ts")));
        assert_eq!(app.resolve_save_path("/x/y"), Some(PathBuf::from("/x/y")));
        assert_eq!(app.resolve_save_path("dir/"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_all_saves_unsaved_pinned_tabs_and_keeps_them_open() {
        let _g = lock();
        let dir = tmp("pinned");
        let mut app = test_app(&dir);
        let other = dir.join("other.txt");
        std::fs::write(&other, "o\n").unwrap();
        app.open_path(&other, None);
        let i = app.docs.iter().position(|d| d.name == "seed.txt").unwrap();
        app.docs[i].pinned = true;
        app.docs[i].replace_text("pinned edit\n");
        // Only the pinned tab is unsaved: Close all still asks (it lists it as a tab that stays).
        app.close_tabs_request(None);
        let Overlay::Ctx(m) = &app.overlay else { panic!("expected the question") };
        let u = m.unsaved.as_ref().unwrap();
        assert_eq!((u.names.clone(), u.kept), (vec!["seed.txt".to_string()], 1));
        app.unsaved_answer(Pending::CloseTabs { keep: None }, Choice::Save);
        assert_eq!(std::fs::read_to_string(dir.join("seed.txt")).unwrap(), "pinned edit\n");
        assert_eq!(app.docs.len(), 1, "the unpinned tab closed, the pinned one stayed");
        assert!(app.docs[0].pinned && !app.docs[0].buf.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
