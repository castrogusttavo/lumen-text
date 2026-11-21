//! The autocomplete menu that opens above the chat composer: slash commands while a `/word` is
//! typed at the start, files while an `@word` is typed anywhere. This is the model (entries,
//! selection, the window of rows shown); drawing and keys are `composer_ui`. Plain code with no
//! App: unit-tested below.

use super::{
    mention,
    slash::{self, Cmd, Handling},
};

/// Rows shown at once (more scroll).
pub const ROWS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    Slash,
    Mention,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Main text (`/resume`, `chat.rs`).
    pub label: String,
    /// What it does / where it is.
    pub detail: String,
    /// Small tag on the right (where a command comes from).
    pub tag: String,
    pub icon: &'static str,
    /// Why it cannot be used here (shown dimmed; picking it explains instead of running).
    pub off: Option<String>,
    /// What picking it puts in the composer (a command's text, a file's relative path).
    pub pick: String,
    /// Picking it right away runs it (a command that takes no arguments).
    pub runs: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Menu {
    pub kind: MenuKind,
    pub entries: Vec<Entry>,
    pub sel: usize,
    /// Where the typed word starts (byte offset of its `/` or `@`).
    pub start: usize,
    pub query: String,
}

impl Menu {
    pub fn up(&mut self) {
        if !self.entries.is_empty() {
            self.sel = if self.sel == 0 { self.entries.len() - 1 } else { self.sel - 1 };
        }
    }

    pub fn down(&mut self) {
        if !self.entries.is_empty() {
            self.sel = (self.sel + 1) % self.entries.len();
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.sel)
    }

    /// The rows to show (`rows` at most) as the index of the first one: the window that keeps the
    /// selection in view.
    pub fn first_row(&self, rows: usize) -> usize {
        if self.entries.len() <= rows {
            return 0;
        }
        (self.sel + 1).saturating_sub(rows).min(self.entries.len() - rows)
    }
}

fn slash_entry(c: &Cmd) -> Entry {
    let off = match &c.handling {
        Handling::Off(why) => Some(why.clone()),
        _ => None,
    };
    let label = if c.hint.is_empty() { format!("/{}", c.name) } else { format!("/{} {}", c.name, c.hint) };
    Entry {
        label,
        detail: c.desc.clone(),
        tag: c.origin.label().to_string(),
        icon: if off.is_some() { "ui/lock" } else { "ui/slash" },
        off,
        pick: slash::completion(c),
        runs: c.hint.is_empty(),
    }
}

/// The menu of slash commands for what is typed, when it is a command name being typed.
pub fn slash(cmds: &[Cmd], text: &str) -> Option<Menu> {
    let q = slash::query(text)?;
    let entries: Vec<Entry> = slash::filter(cmds, q).into_iter().map(slash_entry).collect();
    (!entries.is_empty()).then(|| Menu { kind: MenuKind::Slash, entries, sel: 0, start: 0, query: q.to_string() })
}

/// The menu of files for the `@word` at the caret.
pub fn files(all: &[String], text: &str, caret: usize) -> Option<Menu> {
    let a = mention::active(text, caret)?;
    let entries: Vec<Entry> = mention::rank(all, &a.query, 60)
        .into_iter()
        .map(|rel| {
            let (dir, name) = match rel.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), rel.clone()),
            };
            let icon =
                if mention::image_type(std::path::Path::new(&rel)).is_some() { "ui/image" } else { "ui/file_code" };
            Entry { label: name, detail: dir, tag: String::new(), icon, off: None, pick: rel, runs: false }
        })
        .collect();
    (!entries.is_empty()).then_some(Menu { kind: MenuKind::Mention, entries, sel: 0, start: a.start, query: a.query })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_aipanel::backend::Kind;

    #[test]
    fn the_slash_menu_opens_for_a_command_word_and_marks_what_is_off() {
        let cmds = slash::builtin(Kind::Gemini);
        let m = slash(&cmds, "/res").unwrap();
        assert_eq!(m.kind, MenuKind::Slash);
        assert_eq!(m.entries[0].label, "/resume");
        assert!(m.entries[0].off.as_deref().is_some_and(|w| w.contains("Gemini")), "{:?}", m.entries[0]);
        assert_eq!(m.entries[0].icon, "ui/lock");
        let m = slash(&cmds, "/").unwrap();
        assert!(m.entries.len() > 5);
        let clear = m.entries.iter().find(|e| e.label == "/clear").unwrap();
        assert!(clear.runs && clear.off.is_none() && clear.pick == "/clear");
        let model = m.entries.iter().find(|e| e.label.starts_with("/model")).unwrap();
        assert!(!model.runs, "it takes a name: picking it completes it");
        assert_eq!(model.pick, "/model ");
        assert!(slash(&cmds, "hello").is_none() && slash(&cmds, "/resume now").is_none());
        assert!(slash(&cmds, "/qqqq").is_none(), "nothing matches: no menu");
    }

    #[test]
    fn the_file_menu_follows_the_mention_at_the_caret() {
        let all: Vec<String> = ["src/main.rs", "src/app/chat.rs", "logo.png"].iter().map(|s| s.to_string()).collect();
        let m = files(&all, "look at @ch", 11).unwrap();
        assert_eq!((m.kind, m.start, m.query.as_str()), (MenuKind::Mention, 8, "ch"));
        assert_eq!((m.entries[0].label.as_str(), m.entries[0].detail.as_str()), ("chat.rs", "src/app"));
        assert_eq!(m.entries[0].pick, "src/app/chat.rs");
        let m = files(&all, "@logo", 5).unwrap();
        assert_eq!(m.entries[0].icon, "ui/image");
        assert!(files(&all, "no mention here", 5).is_none());
        assert!(files(&all, "@zzz", 4).is_none());
    }

    #[test]
    fn the_selection_wraps_and_the_window_follows_it() {
        let e = |n: &str| Entry {
            label: n.into(),
            detail: String::new(),
            tag: String::new(),
            icon: "ui/file",
            off: None,
            pick: n.into(),
            runs: false,
        };
        let mut m = Menu {
            kind: MenuKind::Mention,
            entries: (0..12).map(|i| e(&format!("f{i}"))).collect(),
            sel: 0,
            start: 0,
            query: String::new(),
        };
        assert_eq!(m.first_row(ROWS), 0);
        m.up();
        assert_eq!(m.sel, 11, "up from the first goes to the last");
        assert_eq!(m.first_row(ROWS), 4, "the window ends at the selection");
        m.down();
        assert_eq!((m.sel, m.first_row(ROWS)), (0, 0));
        for _ in 0..8 {
            m.down();
        }
        assert_eq!((m.sel, m.first_row(ROWS)), (8, 1));
        assert_eq!(m.selected().unwrap().label, "f8");
        m.entries.truncate(3);
        m.sel = 2;
        assert_eq!(m.first_row(ROWS), 0, "a short list is never scrolled");
    }
}
