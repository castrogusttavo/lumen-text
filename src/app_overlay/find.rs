use super::*;

/// Matches the find bar keeps at most.
const MAX_MATCHES: usize = 10_000;

thread_local! {
    /// The regex of the last find bar query, with what it was compiled from: (text, match case,
    /// regex mode). `None` for a pattern that does not compile. Every edit with the bar open
    /// searches the buffer again; the pattern only changes when the query or an option does.
    static FIND_RE: std::cell::RefCell<Option<((String, bool, bool), Option<regex::Regex>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Patterns compiled by `find_regex` on this thread.
    static COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The regex for a find bar query: `text` as a pattern in regex mode, else as a literal. `None`
/// when it does not compile. Compiled once per (text, case, regex mode).
fn find_regex(text: &str, case: bool, regex: bool) -> Option<regex::Regex> {
    FIND_RE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some((key, re)) = slot.as_ref()
            && key.0 == text
            && key.1 == case
            && key.2 == regex
        {
            return re.clone();
        }
        let pat = if regex { text.to_string() } else { regex::escape(text) };
        let re = RegexBuilder::new(&pat).case_insensitive(!case).build().ok();
        #[cfg(test)]
        COMPILES.with(|c| c.set(c.get() + 1));
        *slot = Some(((text.to_string(), case, regex), re.clone()));
        re
    })
}

/// The char range of every non-empty match of `re` in `buf`, line by line, into `out` (emptied
/// first): at most `cap` of them. One string is reused for every line.
fn scan_matches(buf: &crate::buffer::Buffer, re: &regex::Regex, cap: usize, out: &mut Vec<(usize, usize)>) {
    out.clear();
    let rope = buf.snapshot();
    let mut line = String::new();
    for l in 0..buf.line_count() {
        crate::highlight::rope_line_into(&rope, l, &mut line);
        if line.is_empty() {
            continue; // only an empty match fits, and those are skipped
        }
        let ascii = line.is_ascii();
        let mut start = None;
        for m in re.find_iter(&line) {
            if m.start() == m.end() {
                continue;
            }
            let ls = *start.get_or_insert_with(|| buf.line_start(l));
            let (a, n) = if ascii {
                (m.start(), m.end() - m.start())
            } else {
                (line[..m.start()].chars().count(), line[m.start()..m.end()].chars().count())
            };
            out.push((ls + a, ls + a + n));
            if out.len() >= cap {
                return;
            }
        }
    }
}

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
        // Runs every frame: compared field by field, the query is only copied when it changed.
        let same_query = f.sig.0 == active && f.sig.2 == f.field.text && f.sig.3 == f.case && f.sig.4 == f.regex;
        if same_query && f.sig.1 == d.version {
            return;
        }
        let query_changed = !same_query;
        if query_changed {
            f.sig = (active, d.version, f.field.text.clone(), f.case, f.regex);
        } else {
            f.sig.1 = d.version;
        }
        f.matches.clear();
        f.valid = true;
        if f.field.text.is_empty() {
            f.current = None;
            return;
        }
        let Some(re) = find_regex(&f.field.text, f.case, f.regex) else {
            f.valid = false;
            f.current = None;
            return;
        };
        scan_matches(&d.buf, &re, MAX_MATCHES, &mut f.matches);
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

#[cfg(test)]
mod tests {
    use super::{COMPILES, find_regex, scan_matches};
    use crate::buffer::Buffer;

    fn compiles() -> usize {
        COMPILES.with(|c| c.get())
    }

    /// The scan as it was written before: a `String` per line, chars counted for every match.
    fn per_line(buf: &Buffer, re: &regex::Regex, cap: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        'outer: for l in 0..buf.line_count() {
            let line = buf.line(l);
            let ls = buf.line_start(l);
            for m in re.find_iter(&line) {
                if m.start() == m.end() {
                    continue;
                }
                let a = ls + line[..m.start()].chars().count();
                out.push((a, a + line[m.start()..m.end()].chars().count()));
                if out.len() >= cap {
                    break 'outer;
                }
            }
        }
        out
    }

    #[test]
    fn scan_finds_what_the_per_line_scan_found() {
        let buf = Buffer::from_str("let a = 1;\r\nlét b = a + a;\n\n日本 a 日本a\n\tA_a\nlast a");
        for (query, case, regex) in [
            ("a", false, false),
            ("a", true, false),
            ("a+", false, true),
            ("x*", false, true),
            ("日本", true, false),
            (".", false, false),
            ("\\w+", true, true),
            ("é b", false, false),
        ] {
            let re = find_regex(query, case, regex).unwrap();
            let mut got = vec![(9, 9)]; // left over from an earlier scan: must be dropped
            scan_matches(&buf, &re, 10_000, &mut got);
            assert_eq!(got, per_line(&buf, &re, 10_000), "{query:?}");
            scan_matches(&buf, &re, 3, &mut got);
            assert_eq!(got, per_line(&buf, &re, 3), "{query:?} capped");
            assert!(got.len() <= 3);
        }
        let re = find_regex("a", true, false).unwrap();
        let mut got = Vec::new();
        scan_matches(&buf, &re, 10_000, &mut got);
        assert_eq!(got.len(), 8, "{got:?}");
        assert!(got.iter().all(|&(a, b)| buf.slice(a, b) == "a"));
    }

    #[test]
    fn the_regex_is_compiled_once_per_query_and_options() {
        let n = compiles();
        let literal = find_regex("a.c", false, false).unwrap();
        assert_eq!(compiles(), n + 1);
        assert!(literal.is_match("xA.Cx") && !literal.is_match("abc"));
        find_regex("a.c", false, false).unwrap();
        assert_eq!(compiles(), n + 1, "the same query was compiled twice");
        // Each input of the key compiles a new pattern, and the new pattern is the one returned.
        let pattern = find_regex("a.c", false, true).unwrap();
        assert_eq!(compiles(), n + 2, "regex mode changed");
        assert!(pattern.is_match("ABC"));
        let cased = find_regex("a.c", true, true).unwrap();
        assert_eq!(compiles(), n + 3, "match case changed");
        assert!(cased.is_match("abc") && !cased.is_match("ABC"));
        let other = find_regex("a.d", true, true).unwrap();
        assert_eq!(compiles(), n + 4, "the text changed");
        assert!(other.is_match("abd") && !other.is_match("abc"));
        // A pattern that does not compile is remembered as such, and is fine as a literal.
        assert!(find_regex("(", true, true).is_none());
        assert!(find_regex("(", true, true).is_none());
        assert_eq!(compiles(), n + 5);
        assert!(find_regex("(", true, false).unwrap().is_match("f(x)"));
        assert_eq!(compiles(), n + 6);
    }
}
