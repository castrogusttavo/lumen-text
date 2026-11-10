use super::*;

impl App {
    // ───────────── find / replace ─────────────

    pub fn open_find(&mut self, replace: bool) {
        if self.docs.is_empty() {
            return;
        }
        let seed = {
            let d = &self.docs[self.active];
            let s = d.ed.selected_text(&d.buf);
            (!s.is_empty() && !s.contains('\n')).then_some(s)
        };
        let f = self.find.get_or_insert_with(|| FindBar {
            field: TextField::default(),
            replace: TextField::default(),
            show_replace: false,
            focused: true,
            in_replace: false,
            case: false,
            regex: false,
            matches: Vec::new(),
            current: None,
            valid: true,
            sig: (usize::MAX, -1, String::new(), false, false),
        });
        f.focused = true;
        f.show_replace |= replace;
        f.in_replace = replace;
        if let Some(s) = seed {
            f.field = TextField::with(&s);
        } else {
            f.field.end();
        }
    }

    /// Recompute matches when the query, options, document or text changed.
    pub fn refresh_find(&mut self) {
        let active = self.active;
        let Some(f) = self.find.as_mut() else { return };
        let Some(d) = self.docs.get(active) else { return };
        let sig = (active, d.version, f.field.text.clone(), f.case, f.regex);
        if f.sig == sig {
            return;
        }
        let query_changed = f.sig.2 != sig.2 || f.sig.3 != sig.3 || f.sig.4 != sig.4 || f.sig.0 != sig.0;
        f.sig = sig;
        f.matches.clear();
        f.valid = true;
        if f.field.text.is_empty() {
            f.current = None;
            return;
        }
        let pat = if f.regex { f.field.text.clone() } else { regex::escape(&f.field.text) };
        let Ok(re) = RegexBuilder::new(&pat).case_insensitive(!f.case).build() else {
            f.valid = false;
            f.current = None;
            return;
        };
        'outer: for l in 0..d.buf.line_count() {
            let line = d.buf.line(l);
            let ls = d.buf.line_start(l);
            for m in re.find_iter(&line) {
                if m.start() == m.end() {
                    continue;
                }
                let a = ls + line[..m.start()].chars().count();
                f.matches.push((a, a + line[m.start()..m.end()].chars().count()));
                if f.matches.len() >= 10_000 {
                    break 'outer;
                }
            }
        }
        if query_changed {
            let head = d.ed.sel.start();
            f.current =
                f.matches.iter().position(|&(a, _)| a >= head).or(if f.matches.is_empty() { None } else { Some(0) });
        } else if let Some(c) = f.current {
            f.current = if f.matches.is_empty() { None } else { Some(c.min(f.matches.len() - 1)) };
        }
    }

    pub(super) fn find_jump(&mut self, forward: bool) {
        self.refresh_find();
        let Some(f) = self.find.as_mut() else { return };
        if f.matches.is_empty() {
            return;
        }
        let n = f.matches.len();
        let cur = match f.current {
            Some(c) => {
                if forward {
                    (c + 1) % n
                } else {
                    (c + n - 1) % n
                }
            }
            None => 0,
        };
        f.current = Some(cur);
        let (a, b) = f.matches[cur];
        if let Some(d) = self.docs.get_mut(self.active) {
            d.ed.sel = crate::buffer::Sel { anchor: a, head: b };
            d.reveal = true;
        }
    }

    pub(super) fn replace_one(&mut self) {
        self.refresh_find();
        let Some(f) = self.find.as_ref() else { return };
        let (Some(c), rep) = (f.current, f.replace.text.clone()) else { return };
        let (a, b) = f.matches[c];
        if let Some(d) = self.docs.get_mut(self.active) {
            d.buf.close_group();
            let before = d.ed.sel;
            d.buf.replace(a, b, &rep, EditKind::Other, before);
            d.ed.sel = crate::buffer::Sel::caret(a + rep.chars().count());
            d.buf.set_after(d.ed.sel);
            d.buf.close_group();
            d.sync_edits();
        }
        self.refresh_find();
        self.find_jump(true);
    }

    pub(super) fn replace_all(&mut self) {
        self.refresh_find();
        let Some(f) = self.find.as_ref() else { return };
        if f.matches.is_empty() {
            return;
        }
        let (matches, rep) = (f.matches.clone(), f.replace.text.clone());
        let n = matches.len();
        if let Some(d) = self.docs.get_mut(self.active) {
            let before = d.ed.sel;
            d.buf.begin_compound(before);
            for &(a, b) in matches.iter().rev() {
                d.buf.replace(a, b, &rep, EditKind::Other, before);
            }
            d.buf.end_compound(before);
            d.ed.sel = crate::buffer::Sel::caret(d.ed.sel.head.min(d.buf.len_chars()));
            d.sync_edits();
        }
        self.say(format!("{n} replacements"));
    }

    /// Keys while the find bar has focus. Returns true when consumed.
    pub fn find_key(&mut self, ev: &KeyIn) -> bool {
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let key = ev.key.clone();
        let mut paste = None;
        if ctrl && let Key::Character(c) = &key {
            match c.to_lowercase().as_str() {
                "v" => paste = Some(self.clip.get()),
                "a" => {
                    if let Some(f) = self.find.as_mut() {
                        if f.in_replace { f.replace.select_all() } else { f.field.select_all() }
                    }
                    return false;
                }
                "f" | "h" => return false, // handled globally (re-focus)
                _ => {}
            }
        }
        if let Key::Named(NamedKey::Enter) = &key {
            if alt && ctrl {
                self.replace_all();
            } else if self.find.as_ref().is_some_and(|f| f.in_replace) {
                self.replace_one();
            } else {
                self.find_jump(!shift);
            }
            return true;
        }
        if let Key::Named(NamedKey::F3) = &key {
            self.find_jump(!shift);
            return true;
        }
        let Some(f) = self.find.as_mut() else { return false };
        match &key {
            Key::Named(NamedKey::Escape) => {
                self.find = None;
            }
            Key::Named(NamedKey::Tab) => {
                if f.show_replace {
                    f.in_replace = !f.in_replace;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.find_jump(true);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.find_jump(false);
            }
            Key::Character(c) if alt && (c.as_str() == "c" || c.as_str() == "r") => {
                if c.as_str() == "c" {
                    f.case = !f.case
                } else {
                    f.regex = !f.regex
                }
            }
            _ => {
                let field = if f.in_replace { &mut f.replace } else { &mut f.field };
                match &key {
                    Key::Named(NamedKey::Backspace) => {
                        if ctrl {
                            field.delete_word()
                        } else {
                            field.backspace()
                        };
                    }
                    Key::Named(NamedKey::Delete) => {
                        field.delete();
                    }
                    Key::Named(NamedKey::ArrowLeft) => field.left(),
                    Key::Named(NamedKey::ArrowRight) => field.right(),
                    Key::Named(NamedKey::Home) => field.home(),
                    Key::Named(NamedKey::End) => field.end(),
                    _ => {
                        if let Some(t) = paste {
                            field.insert(&t.replace('\n', " "));
                        } else if !ctrl && !alt {
                            if let Some(t) = &ev.text {
                                field.insert(t);
                            }
                        } else {
                            return false;
                        }
                    }
                }
                // Typing in the search field jumps to the first match from the cursor.
                self.refresh_find();
                let jump = self.find.as_ref().is_some_and(|f| !f.in_replace);
                if jump
                    && let Some(f) = self.find.as_ref()
                    && let Some(c) = f.current
                {
                    let (a, b) = f.matches[c];
                    if let Some(d) = self.docs.get_mut(self.active) {
                        d.ed.sel = crate::buffer::Sel { anchor: a, head: b };
                        d.reveal = true;
                    }
                }
            }
        }
        true
    }
}
