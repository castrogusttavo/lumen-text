use super::*;

// ───────────────────────────── keybindings model ─────────────────────────────

#[derive(Debug, Clone)]
pub struct CommandRow {
    pub id: String,
    pub label: String,
    /// Normalized default combo, e.g. `ctrl+shift+p`.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Mode {
    Browse,
    /// Waiting for the next key combo for the selected command.
    Record,
    /// The captured combo belongs to `other` (a command id): waiting for "Replace".
    Confirm {
        combo: String,
        other: String,
    },
    /// Waiting for confirmation of "Restore defaults".
    ConfirmReset,
}

pub(super) const KB_ROW: i32 = 34;

pub struct KeybindsPage {
    pub(super) commands: Vec<CommandRow>,
    pub(super) defaults: HashMap<String, String>,
    pub(super) user: HashMap<String, String>,
    pub filter: TextField,
    pub(super) filtered: Vec<usize>,
    pub sel: usize,
    pub scroll: f64,
    pub(super) mode: Mode,
    pub(super) note: Option<String>,
    pub(super) view_h: i32,
}

/// Display parts of a normalized combo: "ctrl+shift+p" -> ["ctrl","shift","p"]; "ctrl++" -> ["ctrl","+"];
/// a chord keeps both strokes with a "›" between them ("ctrl+k r" -> ["ctrl","k","›","r"]) and an
/// explorer-only key starts with "explorer" ("explorer:a" -> ["explorer","a"]).
pub fn combo_parts(combo: &str) -> Vec<String> {
    if let Some(rest) = combo.strip_prefix(keymap::EXPLORER) {
        let mut v = vec!["explorer".to_string()];
        v.extend(combo_parts(rest));
        return v;
    }
    if combo.contains(' ') {
        let mut v = Vec::new();
        for (i, stroke) in combo.split(' ').filter(|s| !s.is_empty()).enumerate() {
            if i > 0 {
                v.push("›".to_string());
            }
            v.extend(combo_parts(stroke));
        }
        return v;
    }
    let mut rest = combo;
    let mut out = Vec::new();
    loop {
        let mut found = false;
        for m in ["ctrl+", "shift+", "alt+"] {
            if let Some(r) = rest.strip_prefix(m)
                && !r.is_empty()
            {
                out.push(m.trim_end_matches('+').to_string());
                rest = r;
                found = true;
                break;
            }
        }
        if !found {
            break;
        }
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Key-cap captions for a combo ("ctrl+shift+p" -> ["Ctrl","Shift","P"]).
pub fn combo_chips(combo: &str) -> Vec<String> {
    combo_parts(combo)
        .into_iter()
        .map(|p| -> String {
            match p.as_str() {
                "ctrl" => "Ctrl".into(),
                "shift" => "Shift".into(),
                "alt" => "Alt".into(),
                "explorer" => "Explorer".into(),
                "up" => "↑".into(),
                "down" => "↓".into(),
                "left" => "←".into(),
                "right" => "→".into(),
                "escape" => "Esc".into(),
                "pageup" => "PgUp".into(),
                "pagedown" => "PgDn".into(),
                "delete" => "Del".into(),
                "enter" => "Enter".into(),
                "space" => "Space".into(),
                "tab" => "Tab".into(),
                "backspace" => "Backspace".into(),
                s if s.chars().count() == 1 => s.to_uppercase(),
                s if is_fkey(s) => format!("F{}", &s[1..]),
                s => {
                    let mut ch = s.chars();
                    match ch.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                        None => String::new(),
                    }
                }
            }
        })
        .collect()
}

pub(super) fn is_modifier_name(n: &str) -> bool {
    matches!(
        n,
        "control"
            | "shift"
            | "alt"
            | "altgraph"
            | "super"
            | "meta"
            | "hyper"
            | "capslock"
            | "numlock"
            | "scrolllock"
            | "fn"
            | "fnlock"
            | "symbol"
            | "symbollock"
    )
}

pub(super) fn is_fkey(n: &str) -> bool {
    n.len() > 1 && n.starts_with('f') && n[1..].chars().all(|c| c.is_ascii_digit())
}

impl KeybindsPage {
    pub fn new(commands: Vec<CommandRow>, user: HashMap<String, String>) -> Self {
        let commands: Vec<CommandRow> = commands
            .into_iter()
            .map(|mut c| {
                c.default = c.default.map(|d| keymap::normalize(&d));
                c
            })
            .collect();
        let mut defaults = HashMap::new();
        for c in &commands {
            if let Some(d) = &c.default {
                defaults.insert(d.clone(), c.id.clone());
            }
        }
        let user = user.into_iter().map(|(k, v)| (keymap::normalize(&k), v)).collect();
        let mut p = Self {
            commands,
            defaults,
            user,
            filter: TextField::default(),
            filtered: vec![],
            sel: 0,
            scroll: 0.0,
            mode: Mode::Browse,
            note: None,
            view_h: 340,
        };
        p.refilter();
        p
    }

    pub fn user(&self) -> &HashMap<String, String> {
        &self.user
    }

    /// Register extra built-in combos (aliases such as `ctrl+y` for Redo): they count as defaults
    /// for ownership, conflicts, the "modified" mark and keybindings.json, but the row's main
    /// default stays the one it was created with.
    pub fn add_default_aliases(&mut self, aliases: impl IntoIterator<Item = (String, String)>) {
        for (combo, id) in aliases {
            self.defaults.entry(keymap::normalize(&combo)).or_insert(id);
        }
        self.refilter();
    }

    /// Every built-in combo of a command, main one first.
    pub(super) fn defaults_of(&self, id: &str) -> Vec<String> {
        let main = self.default_of(id).map(str::to_string);
        let mut v: Vec<String> =
            self.defaults.iter().filter(|(_, i)| i.as_str() == id).map(|(c, _)| c.clone()).collect();
        v.sort_by_key(|c| (Some(c) != main.as_ref(), c.clone()));
        v
    }

    /// The command id that owns `combo` right now (user override first, "none" frees it).
    pub fn owner_of(&self, combo: &str) -> Option<&str> {
        match self.user.get(combo) {
            Some(u) if u == "none" => None,
            Some(u) => Some(u.as_str()),
            None => self.defaults.get(combo).map(String::as_str),
        }
    }

    pub(super) fn default_of(&self, id: &str) -> Option<&str> {
        self.commands.iter().find(|c| c.id == id).and_then(|c| c.default.as_deref())
    }

    /// Active combos of a command: its default (unless overridden) first, then user ones sorted.
    pub fn bindings_of(&self, id: &str) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for (c, cmd) in &self.defaults {
            if cmd == id && !self.user.contains_key(c) {
                v.push(c.clone());
            }
        }
        for (c, cmd) in &self.user {
            if cmd == id {
                v.push(c.clone());
            }
        }
        let def = self.default_of(id).map(str::to_string);
        v.sort_by_key(|c| (Some(c) != def.as_ref(), c.clone()));
        v.dedup();
        v
    }

    pub fn is_modified(&self, id: &str) -> bool {
        self.bindings_of(id) != self.defaults_of(id)
    }

    pub(super) fn label_of(&self, id: &str) -> String {
        self.commands.iter().find(|c| c.id == id).map_or_else(|| id.to_string(), |c| c.label.clone())
    }

    /// Rows currently listed (indices into the command list).
    pub fn rows(&self) -> &[usize] {
        &self.filtered
    }

    pub fn row_ids(&self) -> Vec<&str> {
        self.filtered.iter().map(|&i| self.commands[i].id.as_str()).collect()
    }

    pub fn selected(&self) -> Option<usize> {
        self.filtered.get(self.sel).copied()
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected().map(|i| self.commands[i].id.as_str())
    }

    pub fn is_recording(&self) -> bool {
        self.mode == Mode::Record
    }

    /// True while the next key must go to [`KeybindsPage::record_key`] (record / confirm modes).
    pub fn is_capturing(&self) -> bool {
        self.mode != Mode::Browse
    }

    /// `(combo, other command id)` while a "Replace" question is pending.
    pub fn pending_conflict(&self) -> Option<(&str, &str)> {
        match &self.mode {
            Mode::Confirm { combo, other } => Some((combo.as_str(), other.as_str())),
            _ => None,
        }
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub(super) fn refilter(&mut self) {
        let keep = self.selected_id().map(str::to_string);
        let q = self.filter.text.clone();
        let filtered: Vec<usize> = (0..self.commands.len())
            .filter(|&i| {
                let c = &self.commands[i];
                matches_all(&format!("{} {} {}", c.label, c.id, self.bindings_of(&c.id).join(" ")), &q)
            })
            .collect();
        self.filtered = filtered;
        self.sel = keep
            .and_then(|id| self.filtered.iter().position(|&i| self.commands[i].id == id))
            .unwrap_or(0)
            .min(self.filtered.len().saturating_sub(1));
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    pub(super) fn max_scroll(&self) -> f64 {
        (self.filtered.len() as i32 * KB_ROW - self.view_h).max(0) as f64
    }

    pub(super) fn ensure_visible(&mut self) {
        let top = (self.sel as i32 * KB_ROW) as f64;
        let bottom = top + KB_ROW as f64;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.view_h as f64 {
            self.scroll = bottom - self.view_h as f64;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    pub(super) fn move_sel(&mut self, delta: i64) {
        let n = self.filtered.len();
        if n == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
        self.ensure_visible();
    }

    pub fn wheel(&mut self, dy: f64) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
    }

    pub fn select_row(&mut self, k: usize) {
        if k < self.filtered.len() {
            self.sel = k;
            self.ensure_visible();
        }
    }

    /// Enter record mode for the selected command.
    pub fn begin_record(&mut self) {
        if self.selected().is_some() {
            self.mode = Mode::Record;
            self.note = None;
        }
    }

    /// Leave record / confirm mode. True when a mode was left.
    pub fn cancel(&mut self) -> bool {
        let was = self.mode != Mode::Browse;
        self.mode = Mode::Browse;
        self.note = None;
        was
    }

    // --- mutation helpers: each updates `user` and returns the events that mirror the change ---

    pub(super) fn remove_binding(&mut self, id: &str, combo: &str) -> Option<Event> {
        if self.defaults.get(combo).map(String::as_str) == Some(id) {
            // The built-in binding of this very command: disable it explicitly.
            self.user.insert(combo.to_string(), "none".into());
            return Some(Event::Bind { combo: combo.to_string(), command_id: "none".into() });
        }
        if self.user.remove(combo).is_some() {
            return Some(Event::Unbind { combo: combo.to_string() });
        }
        None
    }

    pub(super) fn remove_all(&mut self, id: &str) -> Vec<Event> {
        let mut evs = Vec::new();
        for c in self.bindings_of(id) {
            evs.extend(self.remove_binding(id, &c));
        }
        evs
    }

    /// Give `combo` to `id`, dropping the command's other bindings.
    pub(super) fn apply_bind(&mut self, id: &str, combo: &str) -> Event {
        let mut evs = Vec::new();
        for old in self.bindings_of(id) {
            if old != combo {
                evs.extend(self.remove_binding(id, &old));
            }
        }
        self.user.insert(combo.to_string(), id.to_string());
        evs.push(Event::Bind { combo: combo.to_string(), command_id: id.to_string() });
        self.mode = Mode::Browse;
        self.note = None;
        self.refilter();
        Event::batch(evs)
    }

    /// Remove every binding of the selected command.
    pub fn unbind_selected(&mut self) -> Event {
        let Some(id) = self.selected_id().map(str::to_string) else { return Event::None };
        let evs = self.remove_all(&id);
        self.refilter();
        Event::batch(evs)
    }

    /// Restore the command shown in row `k` to its default binding.
    pub fn reset_row(&mut self, k: usize) -> Event {
        let Some(&i) = self.filtered.get(k) else { return Event::None };
        let id = self.commands[i].id.clone();
        let mut evs = Vec::new();
        let mut combos: Vec<String> = self.user.iter().filter(|(_, v)| **v == id).map(|(k, _)| k.clone()).collect();
        for d in self.defaults_of(&id) {
            if self.user.contains_key(&d) {
                combos.push(d);
            }
        }
        combos.sort();
        combos.dedup();
        for c in combos {
            if self.user.remove(&c).is_some() {
                evs.push(Event::Unbind { combo: c });
            }
        }
        self.refilter();
        Event::batch(evs)
    }

    pub fn reset_all(&mut self) -> Event {
        self.user.clear();
        self.mode = Mode::Browse;
        self.note = None;
        self.refilter();
        Event::ResetAll
    }

    pub fn arm_reset_all(&mut self) {
        self.mode = Mode::ConfirmReset;
        self.note = None;
    }

    /// Accept the pending "Replace" / "Restore defaults" question.
    pub fn confirm(&mut self) -> Event {
        match self.mode.clone() {
            Mode::Confirm { combo, .. } => match self.selected_id().map(str::to_string) {
                Some(id) => self.apply_bind(&id, &combo),
                None => {
                    self.cancel();
                    Event::None
                }
            },
            Mode::ConfirmReset => self.reset_all(),
            _ => Event::None,
        }
    }

    /// Feed the next key combo while recording. Esc cancels; modifier-only presses are ignored;
    /// conflicts switch to the confirm state instead of binding.
    pub fn record_key(&mut self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Event {
        let Some(name) = keymap::key_name(key) else { return Event::None };
        if is_modifier_name(&name) {
            return Event::None;
        }
        let plain = !ctrl && !shift && !alt;
        if plain && name == "escape" {
            self.cancel();
            return Event::None;
        }
        match self.mode {
            Mode::Browse => return Event::None,
            Mode::ConfirmReset => {
                if plain && name == "enter" {
                    return self.reset_all();
                }
                return Event::None;
            }
            Mode::Confirm { .. } if plain && name == "enter" => return self.confirm(),
            _ => {}
        }
        if !(ctrl || alt || is_fkey(&name)) {
            self.note = Some("Use Ctrl or Alt together with the key (or an F key).".into());
            self.mode = Mode::Record;
            return Event::None;
        }
        let combo = keymap::combo(ctrl, shift, alt, &name);
        let Some(id) = self.selected_id().map(str::to_string) else { return Event::None };
        match self.owner_of(&combo).map(str::to_string) {
            Some(o) if o == id => {
                self.mode = Mode::Browse;
                self.note = Some("That shortcut is already assigned to this command.".into());
                Event::None
            }
            Some(o) => {
                self.note = None;
                self.mode = Mode::Confirm { combo, other: o };
                Event::None
            }
            None => self.apply_bind(&id, &combo),
        }
    }

    /// The user map as keybindings.json: only differences from the built-in defaults,
    /// sorted by combo, e.g. `{"ctrl+shift+t":"TestFile","ctrl+w":"none"}`.
    pub fn to_json(&self) -> String {
        let mut m: BTreeMap<&str, &str> = BTreeMap::new();
        for (k, v) in &self.user {
            let same_as_default = self.defaults.get(k) == Some(v);
            let useless_none = v == "none" && !self.defaults.contains_key(k);
            if !same_as_default && !useless_none {
                m.insert(k, v);
            }
        }
        serde_json::to_string(&m).unwrap_or_else(|_| "{}".into())
    }

    pub(super) fn key(&mut self, k: &K, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
        // Browse mode only (capturing modes are routed to `record_key` by the modal).
        match k {
            K::Up => self.move_sel(-1),
            K::Down => self.move_sel(1),
            K::PageUp => self.move_sel(-((self.view_h / KB_ROW).max(1) as i64)),
            K::PageDown => self.move_sel((self.view_h / KB_ROW).max(1) as i64),
            K::Home => self.move_sel(i64::MIN / 2),
            K::End => self.move_sel(i64::MAX / 2),
            K::Enter => self.begin_record(),
            K::Delete => return self.unbind_selected(),
            K::Backspace => {
                if self.filter.text.is_empty() {
                    return self.unbind_selected();
                }
                let ch = if ctrl { self.filter.delete_word() } else { self.filter.backspace() };
                if ch {
                    self.refilter();
                    self.scroll = 0.0;
                }
            }
            K::Left => self.filter.left(),
            K::Right => self.filter.right(),
            K::Esc | K::Tab => {}
            K::Ch(c) if ctrl && c.eq_ignore_ascii_case("r") => {
                if shift {
                    self.arm_reset_all();
                } else {
                    return self.reset_row(self.sel);
                }
            }
            _ => {
                if let Some(t) = typed(k, text, ctrl, alt) {
                    self.filter.insert(&t);
                    self.refilter();
                    self.scroll = 0.0;
                }
            }
        }
        Event::None
    }
}

// ───────────────────────────── key classification ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(super) enum K {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Space,
    Tab,
    Backspace,
    Delete,
    Esc,
    Ch(String),
    Other,
}

pub(super) fn classify(key: &Key) -> K {
    match key {
        Key::Character(c) => K::Ch(c.to_string()),
        Key::Named(n) => match n {
            NamedKey::ArrowUp => K::Up,
            NamedKey::ArrowDown => K::Down,
            NamedKey::ArrowLeft => K::Left,
            NamedKey::ArrowRight => K::Right,
            NamedKey::PageUp => K::PageUp,
            NamedKey::PageDown => K::PageDown,
            NamedKey::Home => K::Home,
            NamedKey::End => K::End,
            NamedKey::Enter => K::Enter,
            NamedKey::Space => K::Space,
            NamedKey::Tab => K::Tab,
            NamedKey::Backspace => K::Backspace,
            NamedKey::Delete => K::Delete,
            NamedKey::Escape => K::Esc,
            _ => K::Other,
        },
        _ => K::Other,
    }
}

/// Printable text produced by a key press (none with Ctrl/Alt).
pub(super) fn typed(k: &K, text: Option<&str>, ctrl: bool, alt: bool) -> Option<String> {
    if ctrl || alt {
        return None;
    }
    let s = match k {
        K::Space => " ".to_string(),
        K::Ch(c) => match text {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => c.clone(),
        },
        _ => return None,
    };
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    (!s.is_empty()).then_some(s)
}
