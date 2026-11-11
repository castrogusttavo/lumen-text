//! Two-button question cards for actions that would lose something the documents do not hold:
//! closing the terminal or quitting while a process still runs ("`npm run dev` is still running"),
//! and the offer to write a file the user may not write ("Save as administrator").
//!
//! The card rides on [`Overlay::Ctx`] like the delete confirmation and the unsaved-changes card do
//! ([`CtxMenu::ask`]), so key/mouse/hover routing and the drawing hook are the context menu's own;
//! the questions themselves are built by pure functions tested below.

use crate::{
    app::App,
    app_ctxmenu::{CtxMenu, Target},
    app_overlay::Overlay,
    ui::{Canvas, Rect},
};
use std::path::PathBuf;
use winit::keyboard::{Key, NamedKey};

/// What "yes" does.
#[derive(Clone, Debug, PartialEq)]
pub enum AskAction {
    /// Kill the integrated terminal with this id (its foreground job goes with it).
    CloseTerminal(u32),
    /// Quit although processes are running.
    Quit,
    /// "Restart to update" although processes are running.
    RestartUpdate,
    /// Write the document with this path through the system's elevation prompt.
    SaveElevated(PathBuf),
}

pub struct Ask {
    pub action: AskAction,
    pub title: String,
    pub body: Vec<String>,
    /// Label of the button that runs the action ("Close anyway", "Save as administrator").
    pub yes_label: &'static str,
    /// The action destroys something: its label is drawn in the error color.
    pub danger: bool,
    /// Focused button: true = the action. Questions about killing processes start on "Cancel".
    pub yes: bool,
    /// Buttons ("Cancel", action) as drawn last frame.
    pub rects: [Rect; 2],
}

impl Ask {
    /// Answer for a key press: `Some(true)` runs the action, `Some(false)` cancels.
    pub fn key(&mut self, key: &Key) -> Option<bool> {
        match key {
            Key::Named(NamedKey::Escape) => Some(false),
            Key::Named(NamedKey::Enter) => Some(self.yes),
            Key::Named(NamedKey::ArrowLeft) => {
                self.yes = false;
                None
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.yes = true;
                None
            }
            Key::Named(NamedKey::Tab) => {
                self.yes = !self.yes;
                None
            }
            Key::Character(s) => match s.to_lowercase().as_str() {
                "y" | "s" => Some(true),
                "n" | "c" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The question asked before running processes are killed by `action`: closing a terminal, or
/// quitting / restarting the editor. `procs` names them (a command line, or a Build & Run job).
pub fn procs_question(procs: &[String], action: AskAction) -> Ask {
    let quitting = !matches!(action, AskAction::CloseTerminal(_));
    let title = match procs {
        [one] => format!("“{one}” is still running"),
        _ => format!("{} processes are still running", procs.len()),
    };
    let mut body = Vec::new();
    if procs.len() > 1 {
        let shown: Vec<String> = procs.iter().take(3).map(|p| format!("“{p}”")).collect();
        let more = if procs.len() > 3 { format!(" and {} more", procs.len() - 3) } else { String::new() };
        body.push(format!("{}{more}", shown.join(", ")));
    }
    let (sentence, yes_label) = if quitting {
        (
            if procs.len() == 1 { "Quitting stops it. Quit anyway?" } else { "Quitting stops them. Quit anyway?" },
            "Quit anyway",
        )
    } else {
        (
            if procs.len() == 1 {
                "Closing the terminal stops it. Close anyway?"
            } else {
                "Closing the terminal stops them. Close anyway?"
            },
            "Close anyway",
        )
    };
    body.push(sentence.into());
    Ask { action, title, body, yes_label, danger: true, yes: false, rects: [Rect::default(); 2] }
}

/// The offer made when saving `path` (shown as `name`) was refused for lack of permission.
pub fn elevated_question(name: &str, path: PathBuf) -> Ask {
    let how = if cfg!(target_os = "macos") {
        "Save as administrator asks for your password, then writes the file in place."
    } else {
        "Save as administrator asks for your password (pkexec), then writes the file in place."
    };
    Ask {
        title: format!("No permission to write “{name}”"),
        body: vec![path.display().to_string(), how.into(), "The file keeps its owner and permissions.".into()],
        action: AskAction::SaveElevated(path),
        yes_label: "Save as administrator",
        danger: false,
        yes: true,
        rects: [Rect::default(); 2],
    }
}

impl App {
    /// Show `ask` as a card over everything.
    pub fn show_ask(&mut self, ask: Ask) {
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
            unsaved: None,
            ask: Some(ask),
        }));
        self.request_redraw();
    }

    /// The question card that is open, if any.
    #[cfg(test)]
    pub fn ask_open(&self) -> Option<&Ask> {
        match &self.overlay {
            Overlay::Ctx(m) => m.ask.as_ref(),
            _ => None,
        }
    }

    /// The card was answered: `yes` runs its action.
    pub fn ask_answer(&mut self, action: AskAction, yes: bool) {
        self.overlay = Overlay::None;
        self.hover_rect = None;
        self.request_redraw();
        if !yes {
            return self.say("cancelled");
        }
        match action {
            AskAction::CloseTerminal(id) => self.close_terminal_id(id),
            AskAction::Quit => {
                self.quit_procs_ok = true;
                if self.try_quit() {
                    self.exit_requested = true;
                }
            }
            AskAction::RestartUpdate => {
                self.quit_procs_ok = true;
                self.update_restart();
            }
            AskAction::SaveElevated(path) => self.save_elevated(&path),
        }
    }

    /// Processes that quitting would kill: the foreground job of every terminal, a running task, a
    /// Build & Run job.
    pub fn running_procs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.terms.tabs.iter().filter_map(|t| t.term.foreground_job()).collect();
        if self.panel.running.is_some() {
            v.push(self.panel.cmd.clone().unwrap_or_else(|| "task".into()));
        }
        if let Some(b) = self.build.running_label() {
            v.push(b);
        }
        v
    }

    /// "Terminal: Kill" on the active terminal (see [`close_terminal_request_id`](Self::close_terminal_request_id)).
    pub fn close_terminal_request(&mut self) {
        self.close_terminal_request_id(self.terms.active);
    }

    /// Kill terminal `id` (command, or the `×` on its tab): asks first when its shell is running a
    /// job, closes right away when idle.
    pub fn close_terminal_request_id(&mut self, id: u32) {
        match self.terms.get(id).and_then(|t| t.term.foreground_job()) {
            Some(job) => self.show_ask(procs_question(&[job], AskAction::CloseTerminal(id))),
            None => self.close_terminal_id(id),
        }
    }

    /// Quitting: true when a card asking about running processes was shown instead (the caller
    /// must not quit yet). Asked once; "Quit anyway" quits without asking again.
    pub fn ask_before_quit(&mut self) -> bool {
        self.ask_procs(AskAction::Quit)
    }

    /// Like [`ask_before_quit`](Self::ask_before_quit), for "Restart to update" (the answer
    /// continues with the restart instead of a plain quit).
    pub fn ask_before_restart(&mut self) -> bool {
        self.ask_procs(AskAction::RestartUpdate)
    }

    fn ask_procs(&mut self, action: AskAction) -> bool {
        if self.quit_procs_ok {
            return false;
        }
        let procs = self.running_procs();
        if procs.is_empty() {
            return false;
        }
        self.show_ask(procs_question(&procs, action));
        true
    }

    /// Key press while the card is open.
    pub fn ask_key(&mut self, key: &Key) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        let Some(a) = m.ask.as_mut() else { return };
        if let Some(yes) = a.key(key) {
            let action = a.action.clone();
            self.ask_answer(action, yes);
        }
        self.request_redraw();
    }

    /// Left click while the card is open: a button answers, a click outside cancels.
    pub fn ask_mouse(&mut self, x: f64, y: f64) {
        let Overlay::Ctx(m) = &mut self.overlay else { return };
        let card = m.card;
        let Some(a) = m.ask.as_ref() else { return };
        let answer = if a.rects[1].contains(x, y) {
            Some(true)
        } else if a.rects[0].contains(x, y) || !card.contains(x, y) {
            Some(false)
        } else {
            None
        };
        if let Some(yes) = answer {
            let action = a.action.clone();
            self.ask_answer(action, yes);
        }
    }

    /// Pointer moved over the card: the hovered button takes the focus. True = redraw.
    pub fn ask_hover(&mut self, x: f64, y: f64) -> bool {
        let Overlay::Ctx(m) = &mut self.overlay else { return false };
        let Some(a) = m.ask.as_mut() else { return false };
        for (i, yes) in [(0, false), (1, true)] {
            if a.rects[i].contains(x, y) && a.yes != yes {
                a.yes = yes;
                return true;
            }
        }
        false
    }

    /// Draw the card (called from `draw_ctxmenu` with the menu taken out of the overlay).
    pub fn draw_ask(&mut self, cv: &mut Canvas, m: &mut CtxMenu) {
        let Some(a) = m.ask.as_mut() else { return };
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let tint = fg.with_alpha(0x1c);
        let sw = self.size.0 as i32;
        let labels = [("Cancel", false), (a.yes_label, true)];
        let widths: Vec<i32> = labels.iter().map(|(l, _)| (self.tx.ui_width(l, false) + 28).max(76)).collect();
        let buttons_w: i32 = widths.iter().sum::<i32>() + 8;
        let w = (sw - 40).clamp(300, 480).max(buttons_w + 24).min((sw - 16).max(200));
        let h = 14 + 26 + a.body.len() as i32 * 20 + 14 + 30 + 12;
        let r = Rect::new((sw - w) / 2, 70, w, h);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        let t = fit(&mut self.tx, &a.title, r.w - 32, true);
        self.tx.draw_ui(cv, r.x + 16, r.y + 14, &t, fg, r, true);
        for (i, line) in a.body.iter().enumerate() {
            let l = fit(&mut self.tx, line, r.w - 32, false);
            self.tx.draw_ui(cv, r.x + 16, r.y + 40 + i as i32 * 20, &l, dim, r, false);
        }
        let bh = 30;
        let by = r.bottom() - bh - 12;
        let mut x = r.right() - 12 - buttons_w;
        for (i, ((label, is_yes), bw)) in labels.iter().zip(&widths).enumerate() {
            let rect = Rect::new(x, by, *bw, bh);
            let focused = a.yes == *is_yes;
            cv.card(rect, 8, c.widget_bg, c.border);
            if focused {
                cv.fill_round(rect.inset(1), 7, tint);
            }
            let col = match (is_yes, a.danger) {
                (true, true) => c.error,
                (true, false) => c.accent_text,
                _ => fg,
            };
            let tw = self.tx.ui_width(label, false);
            self.tx.draw_ui(cv, rect.x + (rect.w - tw) / 2, rect.y + (bh - 18) / 2, label, col, rect, focused);
            a.rects[i] = rect;
            self.layout.overlay_rows.push((rect, i));
            x += bw + 8;
        }
        m.card = r;
    }
}

/// `s` shortened with an ellipsis to fit `max` px.
fn fit(tx: &mut crate::ui::TextCtx, s: &str, max: i32, bold: bool) -> String {
    if tx.ui_width(s, bold) <= max {
        return s.to_string();
    }
    let mut out: String = s.to_string();
    while out.chars().count() > 1 && tx.ui_width(&format!("{out}…"), bold) > max {
        out.pop();
    }
    format!("{out}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn process_questions_name_what_is_running() {
        let a = procs_question(&s(&["npm run dev"]), AskAction::CloseTerminal(2));
        assert_eq!(a.title, "“npm run dev” is still running");
        assert_eq!(a.body, ["Closing the terminal stops it. Close anyway?"]);
        assert_eq!(
            (a.yes_label, a.action.clone(), a.yes, a.danger),
            ("Close anyway", AskAction::CloseTerminal(2), false, true)
        );
        let a = procs_question(&s(&["npm run dev", "my-app (run)", "cargo test", "make"]), AskAction::Quit);
        assert_eq!(a.title, "4 processes are still running");
        assert_eq!(a.body[0], "“npm run dev”, “my-app (run)”, “cargo test” and 1 more");
        assert_eq!(a.body[1], "Quitting stops them. Quit anyway?");
        assert_eq!((a.yes_label, a.action), ("Quit anyway", AskAction::Quit));
    }

    #[test]
    fn keys_answer_or_move_the_focus() {
        let mut a = procs_question(&s(&["sleep 30"]), AskAction::CloseTerminal(1));
        let ch = |c: &str| Key::Character(c.into());
        assert_eq!(a.key(&Key::Named(NamedKey::Enter)), Some(false), "the safe answer is focused first");
        assert_eq!(a.key(&Key::Named(NamedKey::ArrowRight)), None);
        assert_eq!(a.key(&Key::Named(NamedKey::Enter)), Some(true));
        assert_eq!(a.key(&Key::Named(NamedKey::Tab)), None);
        assert!(!a.yes);
        assert_eq!(a.key(&Key::Named(NamedKey::Escape)), Some(false));
        assert_eq!(a.key(&ch("Y")), Some(true));
        assert_eq!(a.key(&ch("n")), Some(false));
        assert_eq!(a.key(&ch("x")), None);
        // Saving as administrator is what the user asked for: Enter confirms.
        let mut e = elevated_question("hosts", "/etc/hosts".into());
        assert_eq!(e.key(&Key::Named(NamedKey::Enter)), Some(true));
        assert_eq!(e.action, AskAction::SaveElevated("/etc/hosts".into()));
        assert!(e.title.contains("hosts") && !e.danger);
    }

    #[cfg(unix)]
    fn shell(dir: &std::path::Path, script: &str) -> crate::terminal::Term {
        let wake: crate::lsp::Wake = std::sync::Arc::new(|| {});
        crate::terminal::Term::spawn_cmd(dir, "sh", &["-m", "-c", script], &[], 10, 60, wake).unwrap()
    }

    #[cfg(unix)]
    fn wait_job(app: &App, id: u32) -> Option<String> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < end {
            if let Some(j) = app.terms.get(id).and_then(|t| t.term.foreground_job()) {
                return Some(j);
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        None
    }

    #[test]
    #[cfg(unix)]
    fn killing_a_terminal_or_quitting_asks_only_while_a_job_runs() {
        let _g = lock();
        let dir = tmp("askterm");
        let mut app = test_app(&dir);
        // An idle shell (waiting for input) closes without a question, and quitting does not ask.
        let idle = app.terms.add(shell(&dir, "read x; true"), false);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(app.running_procs().is_empty());
        assert!(!app.ask_before_quit(), "nothing running: quit goes ahead");
        app.run_cmd(crate::app_overlay::Cmd::CloseTerminal);
        assert!(app.terms.get(idle).is_none() && app.ask_open().is_none(), "idle shell: closed without asking");

        // Two terminals: one runs a job, the other one is idle.
        let busy = app.terms.add(shell(&dir, "sleep 30; true"), false);
        let idle = app.terms.add(shell(&dir, "read x; true"), false);
        assert!(wait_job(&app, busy).is_some_and(|j| j.contains("sleep")));
        // The tab's × on the idle one closes it without asking, even though another one is busy.
        app.close_terminal_request_id(idle);
        assert!(app.terms.get(idle).is_none() && app.ask_open().is_none());
        // The × on the busy one asks, naming the job; Cancel keeps it.
        app.close_terminal_request_id(busy);
        let a = app.ask_open().expect("asks before killing the job");
        assert!(a.title.contains("sleep 30"), "{}", a.title);
        assert_eq!(a.action, AskAction::CloseTerminal(busy));
        app.ask_key(&Key::Named(NamedKey::Escape));
        assert!(app.terms.get(busy).is_some() && app.ask_open().is_none(), "cancel keeps the terminal");

        // Quitting looks at every terminal; "Quit anyway" quits (the event loop exits next turn).
        let idle = app.terms.add(shell(&dir, "read x; true"), false);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(app.running_procs().len(), 1, "only the busy terminal counts");
        assert!(!app.try_quit(), "quit waits for the answer");
        assert_eq!(app.ask_open().map(|a| a.action.clone()), Some(AskAction::Quit));
        app.ask_key(&Key::Named(NamedKey::Escape));
        assert!(!app.exit_requested);
        assert!(!app.try_quit());
        app.ask_key(&Key::Character("y".into()));
        assert!(app.exit_requested, "Quit anyway quits");

        // "Terminal: Kill" acts on the active terminal: "Close anyway" kills the busy one only.
        app.terms.show(busy);
        app.run_cmd(crate::app_overlay::Cmd::CloseTerminal);
        assert_eq!(app.ask_open().map(|a| a.action.clone()), Some(AskAction::CloseTerminal(busy)));
        app.ask_key(&Key::Named(NamedKey::ArrowRight));
        app.ask_key(&Key::Named(NamedKey::Enter));
        assert!(app.terms.get(busy).is_none(), "Close anyway kills the terminal");
        assert!(app.terms.get(idle).is_some(), "the other terminal stays");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
