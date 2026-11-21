//! The native chat on the App: drawing it into the panel's body, and what the pointer and the
//! keys do in it (cards, Allow / Deny, file paths and links, text selection, scrolling).

use super::{
    chat::{self, usage_line},
    chat_view::{self, ChatHit, MdPress},
    *,
};
use crate::ui::fit_ellipsis;

/// Height of the usage strip under the transcript.
const FOOT_H: i32 = 22;

/// A click's outcome that needs the whole App (the chat itself is borrowed while it is decided).
enum After {
    Nothing,
    Copy(String),
    Open(String, Option<usize>),
    Link(String),
    Edit(String),
}

/// Where a link from the transcript points: a web address, or a file with an optional line.
#[derive(Debug, PartialEq)]
pub(super) enum LinkTarget {
    Web(String),
    File(String, Option<usize>),
}

pub(super) fn link_target(url: &str) -> LinkTarget {
    let url = url.trim();
    let scheme =
        url.find(':').filter(|&i| i > 1 && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    if scheme.is_some() {
        return LinkTarget::Web(url.to_string());
    }
    let path = url.split('#').next().unwrap_or("");
    let (p, line) = chat::split_line_suffix(path);
    LinkTarget::File(p.to_string(), line)
}

impl App {
    /// The session shown is a native chat.
    pub(crate) fn ai_is_chat(&self) -> bool {
        self.ai.cur().is_some_and(|s| s.chat.is_some())
    }

    /// The transcript, its usage strip, into `body` (the area above the composer).
    pub(super) fn ai_draw_chat(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        cv.fill(body, self.theme.background);
        let mouse = self.mouse;
        let i = self.ai.active;
        let Some(s) = self.ai.sessions.get_mut(i) else { return };
        let (icon, name, cwd) = (s.agent.icon, s.agent.name, s.cwd.clone());
        let Some(c0) = s.chat.as_mut() else { return };
        let cs: &mut ChatSession = c0;
        let clock = cs.started.elapsed().as_secs_f32();
        let mut foot = usage_line(&cs.chat.usage());
        if cs.chat.context_window > 0 && cs.chat.context_used > 0 {
            let ctx = format!(
                "{:.0}% of {} context",
                cs.chat.context_fraction() * 100.0,
                chat::format_tokens(cs.chat.context_window)
            );
            foot = if foot.is_empty() { ctx } else { format!("{foot} · {ctx}") };
        }
        let view_body =
            if foot.is_empty() { body } else { Rect::new(body.x, body.y, body.w, (body.h - FOOT_H).max(0)) };
        cs.view.draw(cv, &mut self.tx, &self.theme, view_body, &cs.chat, (name, icon), &cwd, mouse, clock);
        if !foot.is_empty() {
            let w = (body.w - 32).clamp(60, chat_view::MAX_COL);
            let x = body.x + (body.w - w) / 2;
            let y = body.bottom() - FOOT_H;
            cv.fill(Rect::new(body.x, y, body.w, 1), c.border);
            let text = fit_ellipsis(&mut self.tx, &foot, w, false);
            let r = Rect::new(x, y + 1, w, FOOT_H - 1);
            self.tx.draw_ui_sized(cv, x, y + 4, &text, c.tab_inactive_fg, r, false, 12.0);
        }
    }

    /// The text selected in the transcript.
    pub(super) fn chat_selected_text(&self) -> Option<String> {
        self.ai.cur()?.chat.as_ref()?.view.selected_text()
    }

    /// PageUp / PageDown / Ctrl+End scroll the transcript (while the composer is empty of ideas
    /// for them). True when the key was one of those.
    pub(super) fn chat_scroll_key(&mut self, key: &Key, ctrl: bool) -> bool {
        use winit::keyboard::NamedKey as N;
        let Some(c) = self.ai.cur_mut().and_then(|s| s.chat.as_mut()) else { return false };
        let page = (c.view.view_h as f64 * 0.85).max(40.0);
        match key {
            Key::Named(N::PageUp) => c.view.scroll_by(-page),
            Key::Named(N::PageDown) => c.view.scroll_by(page),
            Key::Named(N::End) if ctrl => c.view.jump_to_end(),
            _ => return false,
        }
        self.request_redraw();
        true
    }

    /// A left press in the transcript. True when it was on something of the chat.
    pub(super) fn chat_press(&mut self, x: f64, y: f64) -> bool {
        let i = self.ai.active;
        let Some(s) = self.ai.sessions.get_mut(i) else { return false };
        let cwd = s.cwd.clone();
        let Some(c0) = s.chat.as_mut() else { return false };
        let cs: &mut ChatSession = c0;
        let Some((_, hit)) = cs.view.hit_at(x, y) else { return false };
        let after = match hit {
            ChatHit::Toggle(k) => {
                cs.view.toggle(&k);
                After::Nothing
            }
            ChatHit::ToEnd => {
                cs.view.jump_to_end();
                After::Nothing
            }
            ChatHit::Perm(id, allow) => {
                cs.be.answer(&mut cs.chat, &id, allow);
                After::Nothing
            }
            ChatHit::CopyMsg(mi) => After::Copy(chat_view::message_text(&cs.chat, mi)),
            ChatHit::Edit(mi) => After::Edit(chat_view::message_text(&cs.chat, mi)),
            ChatHit::File(p, line) => After::Open(p, line),
            ChatHit::Md(key) => match cs.view.md_press(&mut self.tx, key, x, y) {
                MdPress::Link(u) => After::Link(u),
                MdPress::Copy(t) => After::Copy(t),
                MdPress::Nothing | MdPress::Toggled | MdPress::Selecting => After::Nothing,
            },
        };
        match after {
            After::Nothing => {}
            After::Copy(t) => {
                self.clip.set(&t);
                self.say("Copied");
            }
            After::Edit(t) => self.ai_edit_message(&t),
            After::Open(p, line) => self.chat_open_file(&p, &cwd, line),
            After::Link(u) => match link_target(&u) {
                LinkTarget::Web(w) => crate::paths::open_url(&w),
                LinkTarget::File(p, line) => self.chat_open_file(&p, &cwd, line),
            },
        }
        self.request_redraw();
        true
    }

    /// Open a file named in the transcript (relative to the session's folder) in the editor.
    fn chat_open_file(&mut self, path: &str, cwd: &Path, line: Option<usize>) {
        let p = Path::new(path);
        let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
        if !full.is_file() {
            return self.say(format!("File not found: {}", full.display()));
        }
        self.push_nav();
        self.open_path(&full, line.map(|l| (l.saturating_sub(1), 0)));
    }

    /// Wheel over the transcript. True when it scrolled.
    pub(super) fn chat_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let px = crate::scroller::wheel_px(delta, crate::ui::ui_line() as f64);
        let Some(c) = self.ai.cur_mut().and_then(|s| s.chat.as_mut()) else { return false };
        c.view.scroll_by(-px);
        self.request_redraw();
        true
    }

    /// The pointer moved over the panel: a drag extends the selection; a new hover redraws.
    pub(super) fn chat_move(&mut self, x: f64, y: f64) -> bool {
        if !self.ai.open {
            return false;
        }
        let i = self.ai.active;
        let Some(c0) = self.ai.sessions.get_mut(i).and_then(|s| s.chat.as_mut()) else { return false };
        let cs: &mut ChatSession = c0;
        if cs.view.dragging {
            return cs.view.md_drag(&mut self.tx, x, y);
        }
        cs.view.moved(x, y)
    }

    pub(super) fn chat_release(&mut self) {
        if let Some(c) = self.ai.cur_mut().and_then(|s| s.chat.as_mut()) {
            c.view.dragging = false;
        }
    }

    /// Pointer shape over the transcript (`None` when it is not over it).
    pub(super) fn chat_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let c = self.ai.cur()?.chat.as_ref()?;
        if !self.ai.open || !c.view.body.contains(x, y) {
            return None;
        }
        Some(match c.view.cursor_at(x, y) {
            Cur::Default => CursorIcon::Default,
            Cur::Pointer => CursorIcon::Pointer,
            Cur::Text => CursorIcon::Text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_web_addresses_or_files_with_a_line() {
        assert_eq!(link_target("https://a.b/c"), LinkTarget::Web("https://a.b/c".into()));
        assert_eq!(link_target("mailto:x@y.z"), LinkTarget::Web("mailto:x@y.z".into()));
        assert_eq!(link_target("src/a.rs:12"), LinkTarget::File("src/a.rs".into(), Some(12)));
        assert_eq!(link_target("src/a.rs"), LinkTarget::File("src/a.rs".into(), None));
        assert_eq!(link_target("docs/x.md#intro"), LinkTarget::File("docs/x.md".into(), None));
    }
}
