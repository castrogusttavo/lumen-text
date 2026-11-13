//! The app side of remote development over SSH (see `remote`): the "Open remote folder…" prompt,
//! reacting to what the connection threads report, the save upload and the reconnect commands.

use crate::{
    app::App,
    app_overlay::{Overlay, Prompt, PromptKind},
    field::TextField,
    remote::{self, Event, Target},
};
use std::path::Path;

impl App {
    /// "Remote: Open folder over SSH…": ask for `ssh://user@host/path` (or a host of `~/.ssh/config`).
    pub fn remote_open_prompt(&mut self) {
        let hosts = remote::ssh_config_hosts();
        let mut title = "Open remote folder (ssh://user@host/path, user@host:/path or a host alias)".to_string();
        if !hosts.is_empty() {
            let shown: Vec<&str> = hosts.iter().take(8).map(String::as_str).collect();
            let more = if hosts.len() > shown.len() { ", …" } else { "" };
            title = format!("{title} — in ~/.ssh/config: {}{more}", shown.join(", "));
        }
        let mut field = TextField::with("ssh://");
        field.select_all();
        self.overlay = Overlay::Prompt(Prompt::new(PromptKind::RemoteOpen, title, field));
    }

    /// The address was typed: connect on a thread; the project opens when it answers.
    pub fn remote_open_answer(&mut self, text: &str) {
        match Target::parse(text) {
            Ok(t) => {
                remote::set_wake(self.wake());
                self.say(format!("connecting to {}…", t.label()));
                remote::connect_async(t);
            }
            Err(e) => self.say(format!("remote: {e}")),
        }
    }

    /// Called from the background pump: apply what the connection threads reported.
    pub fn pump_remote(&mut self) {
        let root = self.root.clone();
        remote::adopt_once(&root);
        remote::tick(&root);
        for e in remote::take_events() {
            match e {
                Event::Connected(dir) => self.switch_project(dir),
                Event::Failed(why) => self.say_for(format!("remote: could not connect: {why}"), 6),
                Event::Listed => {
                    self.tree.rebuild();
                    self.request_redraw();
                }
                Event::Link(msg) => {
                    self.say_for(msg, 4);
                    self.request_redraw();
                }
            }
        }
    }

    /// After a local save of document `i`: upload it when it belongs to a remote folder. On failure
    /// the document stays modified (the remote copy is not up to date) and the reason is returned.
    pub fn remote_push(&mut self, i: usize) -> Result<(), String> {
        let Some(path) = self.docs.get(i).and_then(|d| d.path().map(Path::to_path_buf)) else { return Ok(()) };
        match remote::push_saved(&path) {
            None | Some(Ok(())) => Ok(()),
            Some(Err(e)) => {
                if let Some(d) = self.docs.get_mut(i) {
                    d.buf.mark_saved(u64::MAX);
                }
                Err(e.message())
            }
        }
    }

    /// "Remote: Save and overwrite the remote file": upload even though it changed over there.
    pub fn remote_save_overwrite(&mut self) {
        let remote_doc = self.docs.get(self.active).and_then(|d| d.path()).is_some_and(remote::is_remote);
        if !remote_doc {
            return self.say("this file is not on a remote host");
        }
        remote::force_next_push();
        self.save_active();
    }

    /// "Remote: Reconnect": drop the shared SSH connection and connect again.
    pub fn remote_reconnect(&mut self) {
        remote::set_wake(self.wake());
        if remote::reconnect_async(&self.root) {
            self.say("reconnecting…");
        } else {
            self.say("this project is not on a remote host");
        }
    }
}
