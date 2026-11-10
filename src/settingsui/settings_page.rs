use super::*;

pub(super) const SET_ROW: i32 = 60;
pub(super) const DROP_VIS: usize = 8;

#[derive(Debug, Clone)]
pub(super) struct DropState {
    pub(super) def: usize,
    pub(super) sel: usize,
    pub(super) scroll: usize,
}

pub struct SettingsPage {
    pub defs: Vec<SettingDef>,
    pub(super) values: Map<String, Value>,
    pub(super) defaults: Map<String, Value>,
    pub(super) choices: HashMap<String, Vec<String>>,
    pub search: TextField,
    /// Index into [`SettingsPage::sections`].
    pub section: usize,
    /// Index into [`SettingsPage::visible`].
    pub sel: usize,
    pub scroll: f64,
    pub(super) edit: Option<(usize, TextField)>,
    pub(super) drop: Option<DropState>,
    pub(super) view_h: i32,
}

impl SettingsPage {
    pub fn new(defs: Vec<SettingDef>, values: Value) -> Self {
        let values = match values {
            Value::Object(o) => o,
            _ => Map::new(),
        };
        let defaults = match default_values() {
            Value::Object(o) => o,
            _ => Map::new(),
        };
        Self {
            defs,
            values,
            defaults,
            choices: HashMap::new(),
            search: TextField::default(),
            section: 0,
            sel: 0,
            scroll: 0.0,
            edit: None,
            drop: None,
            view_h: 380,
        }
    }

    /// Replace the per-key defaults used for the reset icon.
    pub fn set_defaults(&mut self, defaults: Value) {
        if let Value::Object(o) = defaults {
            self.defaults = o;
        }
    }

    /// Fill the options of a runtime `Choice` (theme lists).
    pub fn set_choices(&mut self, key: &str, choices: Vec<String>) {
        self.choices.insert(key.to_string(), choices);
    }

    pub fn values(&self) -> Value {
        Value::Object(self.values.clone())
    }

    /// Replace the displayed values (after the app changed several at once, e.g. item order).
    pub fn set_values(&mut self, values: Value) {
        if let Value::Object(o) = values {
            self.values = o;
        }
    }

    /// Sections in definition order, without duplicates.
    pub fn sections(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for d in &self.defs {
            if !out.contains(&d.section) {
                out.push(d.section);
            }
        }
        out
    }

    pub(super) fn searching(&self) -> bool {
        !self.search.text.trim().is_empty()
    }

    /// Indices into `defs` of the rows currently listed: the matches of the search box across
    /// all sections, or every row of the selected section.
    pub fn visible(&self) -> Vec<usize> {
        if self.searching() {
            let q = &self.search.text;
            (0..self.defs.len())
                .filter(|&i| matches_all(&format!("{} {}", self.defs[i].label, self.defs[i].help), q))
                .collect()
        } else {
            let secs = self.sections();
            let Some(s) = secs.get(self.section) else { return vec![] };
            (0..self.defs.len()).filter(|&i| self.defs[i].section == *s).collect()
        }
    }

    pub fn selected_def(&self) -> Option<usize> {
        self.visible().get(self.sel).copied()
    }

    pub fn choices_of(&self, def: usize) -> Vec<String> {
        let d = &self.defs[def];
        if let Some(v) = self.choices.get(d.key) {
            return v.clone();
        }
        match &d.kind {
            SettingKind::Choice(o) => o.iter().map(|s| s.to_string()).collect(),
            _ => vec![],
        }
    }

    pub(super) fn choice_count(&self, def: usize) -> usize {
        match self.choices.get(self.defs[def].key) {
            Some(v) => v.len(),
            None => match &self.defs[def].kind {
                SettingKind::Choice(o) => o.len(),
                _ => 0,
            },
        }
    }

    /// Current value of a row (stored value, else default, else the kind's neutral value).
    pub fn current(&self, def: usize) -> Value {
        let d = &self.defs[def];
        if let Some(v) = self.values.get(d.key) {
            return v.clone();
        }
        if let Some(v) = self.defaults.get(d.key) {
            return v.clone();
        }
        match &d.kind {
            SettingKind::Bool => json!(false),
            SettingKind::Choice(o) => json!(o.first().copied().unwrap_or("")),
            SettingKind::Number { min, .. } => num_value(*min),
            SettingKind::Text => json!(""),
        }
    }

    pub fn is_modified(&self, def: usize) -> bool {
        self.defaults.get(self.defs[def].key).is_some_and(|d| !same_value(d, &self.current(def)))
    }

    pub(super) fn cur_bool(&self, def: usize) -> bool {
        self.current(def).as_bool().unwrap_or(false)
    }
    pub(super) fn cur_num(&self, def: usize) -> f64 {
        self.current(def).as_f64().unwrap_or(0.0)
    }
    pub(super) fn cur_str(&self, def: usize) -> String {
        match self.current(def) {
            Value::String(s) => s,
            Value::Null => String::new(),
            o => o.to_string(),
        }
    }

    pub(super) fn explicit(&self, def: usize) -> bool {
        self.values.contains_key(self.defs[def].key)
    }

    /// Store `value` and report it, unless the stored value is already equal.
    pub(super) fn set_value(&mut self, def: usize, value: Value) -> Event {
        if self.explicit(def) && self.current(def) == value {
            return Event::None;
        }
        let key = self.defs[def].key;
        self.values.insert(key.to_string(), value.clone());
        Event::Setting { key: key.to_string(), value }
    }

    pub fn set_bool(&mut self, def: usize, on: bool) -> Event {
        if !matches!(self.defs[def].kind, SettingKind::Bool) {
            return Event::None;
        }
        self.set_value(def, json!(on))
    }

    pub fn toggle(&mut self, def: usize) -> Event {
        if !matches!(self.defs[def].kind, SettingKind::Bool) {
            return Event::None;
        }
        let on = !self.cur_bool(def);
        self.set_value(def, json!(on))
    }

    /// Select the next (`dir > 0`) or previous option, wrapping around.
    pub fn cycle(&mut self, def: usize, dir: i32) -> Event {
        let opts = self.choices_of(def);
        if opts.is_empty() {
            return Event::None;
        }
        let cur = self.cur_str(def);
        let n = opts.len() as i32;
        let idx = match opts.iter().position(|o| *o == cur) {
            Some(i) => (i as i32 + dir.signum()).rem_euclid(n),
            None if dir >= 0 => 0,
            None => n - 1,
        };
        self.pick(def, idx as usize)
    }

    pub fn pick(&mut self, def: usize, option: usize) -> Event {
        let opts = self.choices_of(def);
        match opts.get(option) {
            Some(o) => self.set_value(def, json!(o)),
            None => Event::None,
        }
    }

    /// Add `dir * step` to a number row, clamped to its range.
    pub fn step(&mut self, def: usize, dir: i32) -> Event {
        let SettingKind::Number { step, .. } = self.defs[def].kind else { return Event::None };
        let v = self.cur_num(def) + dir.signum() as f64 * step;
        self.set_number(def, v)
    }

    /// Set a number row to `v`, clamped to `min..=max`. No event when nothing changes.
    pub fn set_number(&mut self, def: usize, v: f64) -> Event {
        let SettingKind::Number { min, max, .. } = self.defs[def].kind else { return Event::None };
        let v = v.clamp(min, max);
        if (self.cur_num(def) - v).abs() < 1e-9 && self.current(def).is_number() {
            return Event::None;
        }
        self.set_value(def, num_value(v))
    }

    pub fn set_text(&mut self, def: usize, s: &str) -> Event {
        if !matches!(self.defs[def].kind, SettingKind::Text) {
            return Event::None;
        }
        self.set_value(def, json!(s))
    }

    /// Put a row back to its default value.
    pub fn reset_def(&mut self, def: usize) -> Event {
        match self.defaults.get(self.defs[def].key).cloned() {
            Some(v) if v != self.current(def) => self.set_value(def, v),
            _ => Event::None,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn dropdown_open(&self) -> bool {
        self.drop.is_some()
    }

    pub(super) fn start_edit(&mut self, def: usize) {
        self.drop = None;
        self.edit = Some((def, TextField::with(&self.cur_str(def))));
    }

    pub(super) fn commit_edit(&mut self) -> Event {
        match self.edit.take() {
            Some((def, f)) => self.set_text(def, &f.text),
            None => Event::None,
        }
    }

    pub(super) fn open_drop(&mut self, def: usize) {
        let opts = self.choices_of(def);
        if opts.is_empty() {
            return;
        }
        let cur = self.cur_str(def);
        let sel = opts.iter().position(|o| *o == cur).unwrap_or(0);
        let scroll = sel.saturating_sub(DROP_VIS / 2).min(opts.len().saturating_sub(DROP_VIS));
        self.edit = None;
        self.drop = Some(DropState { def, sel, scroll });
    }

    pub(super) fn drop_move(&mut self, delta: i64) {
        let Some(def) = self.drop.as_ref().map(|d| d.def) else { return };
        let n = self.choice_count(def);
        if n == 0 {
            return;
        }
        if let Some(d) = self.drop.as_mut() {
            d.sel = (d.sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
            if d.sel < d.scroll {
                d.scroll = d.sel;
            } else if d.sel >= d.scroll + DROP_VIS {
                d.scroll = d.sel + 1 - DROP_VIS;
            }
        }
    }

    /// Enter / click on the row's main control.
    pub fn activate(&mut self, def: usize) -> Event {
        match self.defs[def].kind.clone() {
            SettingKind::Bool => self.toggle(def),
            SettingKind::Choice(_) => {
                if is_segmented(self.choice_count(def)) {
                    self.cycle(def, 1)
                } else {
                    self.open_drop(def);
                    Event::None
                }
            }
            SettingKind::Text => {
                if let Some(key) = crate::settings_ext::picker_key(self.defs[def].key) {
                    return Event::PickFont { key: key.into() };
                }
                self.start_edit(def);
                Event::None
            }
            SettingKind::Number { .. } => Event::None,
        }
    }

    pub fn select_def(&mut self, def: usize) {
        if let Some(k) = self.visible().iter().position(|&i| i == def) {
            self.sel = k;
            self.ensure_visible();
        }
    }

    pub fn set_section(&mut self, i: usize) {
        if i < self.sections().len() {
            self.section = i;
            self.sel = 0;
            self.scroll = 0.0;
            self.edit = None;
            self.drop = None;
        }
    }

    pub(super) fn max_scroll(&self) -> f64 {
        (self.visible().len() as i32 * SET_ROW - self.view_h).max(0) as f64
    }

    pub(super) fn ensure_visible(&mut self) {
        let top = (self.sel as i32 * SET_ROW) as f64;
        let bottom = top + SET_ROW as f64;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.view_h as f64 {
            self.scroll = bottom - self.view_h as f64;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    pub(super) fn move_sel(&mut self, delta: i64) {
        let n = self.visible().len();
        if n == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
        self.ensure_visible();
    }

    pub(super) fn page_rows(&self) -> i64 {
        ((self.view_h / SET_ROW).max(1)) as i64
    }

    pub(super) fn search_changed(&mut self) {
        self.sel = 0;
        self.scroll = 0.0;
    }

    /// Cancel an open dropdown / text edit. True when something was cancelled.
    pub fn cancel(&mut self) -> bool {
        if self.drop.take().is_some() {
            return true;
        }
        self.edit.take().is_some()
    }

    /// Scroll by `dy` pixels (the dropdown scrolls by one option per notch).
    pub fn wheel(&mut self, dy: f64) {
        if let Some(def) = self.drop.as_ref().map(|d| d.def) {
            let max = self.choice_count(def).saturating_sub(DROP_VIS) as i64;
            if let Some(d) = self.drop.as_mut() {
                let step = if dy > 0.0 {
                    1
                } else if dy < 0.0 {
                    -1
                } else {
                    0
                };
                d.scroll = (d.scroll as i64 + step).clamp(0, max) as usize;
            }
            return;
        }
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
    }

    pub(super) fn key(&mut self, k: &K, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
        // Inline text edit.
        if self.edit.is_some() {
            match k {
                K::Enter => return self.commit_edit(),
                K::Esc => {
                    self.edit = None;
                }
                _ => {
                    let f = &mut self.edit.as_mut().unwrap().1;
                    match k {
                        K::Left => f.left(),
                        K::Right => f.right(),
                        K::Home => f.home(),
                        K::End => f.end(),
                        K::Backspace => {
                            if ctrl {
                                f.delete_word();
                            } else {
                                f.backspace();
                            }
                        }
                        K::Delete => {
                            f.delete();
                        }
                        _ => {
                            if let Some(t) = typed(k, text, ctrl, alt) {
                                f.insert(&t);
                            }
                        }
                    }
                }
            }
            return Event::None;
        }
        // Dropdown list.
        if let Some(d) = self.drop.clone() {
            match k {
                K::Up => self.drop_move(-1),
                K::Down => self.drop_move(1),
                K::PageUp => self.drop_move(-(DROP_VIS as i64)),
                K::PageDown => self.drop_move(DROP_VIS as i64),
                K::Home => self.drop_move(i64::MIN / 2),
                K::End => self.drop_move(i64::MAX / 2),
                K::Esc => self.drop = None,
                K::Enter | K::Space => {
                    let sel = self.drop.as_ref().map_or(d.sel, |x| x.sel);
                    self.drop = None;
                    return self.pick(d.def, sel);
                }
                _ => {}
            }
            return Event::None;
        }
        let empty_search = !self.searching();
        match k {
            K::Tab => {
                let n = self.sections().len();
                if n > 0 {
                    let next = if shift { (self.section + n - 1) % n } else { (self.section + 1) % n };
                    self.search = TextField::default();
                    self.set_section(next);
                }
            }
            K::Up => self.move_sel(-1),
            K::Down => self.move_sel(1),
            K::PageUp => self.move_sel(-self.page_rows()),
            K::PageDown => self.move_sel(self.page_rows()),
            K::Home => self.move_sel(i64::MIN / 2),
            K::End => self.move_sel(i64::MAX / 2),
            K::Left | K::Right => {
                let dir = if *k == K::Left { -1 } else { 1 };
                match self.selected_def().map(|d| (d, self.defs[d].kind.clone())) {
                    Some((d, SettingKind::Bool)) => return self.set_bool(d, dir > 0),
                    Some((d, SettingKind::Choice(_))) => return self.cycle(d, dir),
                    Some((d, SettingKind::Number { .. })) => return self.step(d, dir),
                    _ => {
                        if dir < 0 {
                            self.search.left()
                        } else {
                            self.search.right()
                        }
                    }
                }
            }
            K::Enter => {
                if let Some(d) = self.selected_def() {
                    return self.activate(d);
                }
            }
            K::Space if empty_search => {
                if let Some(d) = self.selected_def()
                    && matches!(self.defs[d].kind, SettingKind::Bool | SettingKind::Choice(_))
                {
                    return self.activate(d);
                }
            }
            K::Backspace => {
                let ch = if ctrl { self.search.delete_word() } else { self.search.backspace() };
                if ch {
                    self.search_changed();
                }
            }
            K::Delete => {
                if self.search.delete() {
                    self.search_changed();
                }
            }
            K::Esc => {
                self.cancel();
            }
            _ => {
                if let Some(t) = typed(k, text, ctrl, alt) {
                    self.search.insert(&t);
                    self.search_changed();
                }
            }
        }
        Event::None
    }
}
