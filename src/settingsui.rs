//! Modal with two pages: SETTINGS (searchable list of config keys with toggles, choosers,
//! steppers and text fields) and KEYBINDINGS (filterable command table with key-cap chips and a
//! record mode with conflict detection).
//!
//! Everything here is plain state + drawing: input methods return an [`Event`] that the app
//! applies (write config.json / keybindings.json, reload the keymap, ...). Nothing touches `App`.
//!
//! Hit-testing uses the rectangles recorded by the last `draw`, so `mouse_down` is only
//! meaningful after a frame was drawn (hover is not tracked).
#![allow(dead_code)]

use crate::{
    field::TextField,
    keymap,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx},
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use winit::keyboard::{Key, NamedKey};

// ───────────────────────────── events ─────────────────────────────

/// What the app must do after an input call.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    None,
    /// Close the modal.
    Close,
    /// A setting changed: store `value` under `key` in config.json and apply it.
    Setting { key: String, value: Value },
    /// Set `keybindings.json[combo] = command_id` (`command_id == "none"` disables a default).
    Bind { combo: String, command_id: String },
    /// Remove `combo` from keybindings.json (the built-in default, if any, applies again).
    Unbind { combo: String },
    /// Clear keybindings.json entirely (every shortcut back to its default).
    ResetAll,
    /// Several events produced by one action, to be applied in order.
    Batch(Vec<Event>),
}

impl Event {
    /// Flatten `Batch` recursively into a plain list (never contains `None` or `Batch`).
    pub fn flatten(self) -> Vec<Event> {
        match self {
            Event::None => vec![],
            Event::Batch(v) => v.into_iter().flat_map(Event::flatten).collect(),
            e => vec![e],
        }
    }

    fn batch(mut v: Vec<Event>) -> Event {
        match v.len() {
            0 => Event::None,
            1 => v.remove(0),
            _ => Event::Batch(v),
        }
    }
}

// ───────────────────────────── settings model ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    /// Fixed options; an empty slice means "filled at runtime" via [`SettingsPage::set_choices`].
    Choice(&'static [&'static str]),
    Number { min: f64, max: f64, step: f64 },
    Text,
}

#[derive(Debug, Clone)]
pub struct SettingDef {
    pub key: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
}

/// Keys that are listed here but NOT yet read/written by `config.rs`; the owner must wire them.
pub const PENDING_KEYS: &[&str] = &["minimap", "word_wrap", "sticky_scroll", "indent_guides"];

pub fn default_settings() -> Vec<SettingDef> {
    use SettingKind::*;
    let d = |key, section, label, help, kind| SettingDef { key, section, label, help, kind };
    vec![
        d("format_on_save", "Geral", "Formatar ao salvar", "Roda o formatador do servidor de linguagem sempre que um arquivo é salvo.", Bool),
        d("font_size", "Editor", "Tamanho da fonte", "Tamanho do texto do código em pixels (8 a 40). Ctrl + roda do mouse ajusta temporariamente.", Number { min: 8.0, max: 40.0, step: 1.0 }),
        // --- not in config.rs yet (see PENDING_KEYS) ---
        d("minimap", "Editor", "Minimapa", "Mostra a miniatura do arquivo na borda direita do editor.", Bool),
        d("minimap_style", "Editor", "Estilo do minimapa", "Sutil é mais estreito e de baixo contraste; Normal é a aparência original.", Choice(&["subtle", "normal"])),
        d("word_wrap", "Editor", "Quebra de linha", "Quebra linhas longas na largura da janela em vez de rolar na horizontal.", Bool),
        d("sticky_scroll", "Editor", "Rolagem fixa", "Mantém os escopos (função, classe) visíveis no topo enquanto você rola.", Bool),
        d("indent_guides", "Editor", "Guias de indentação", "Desenha linhas verticais finas nos níveis de indentação.", Bool),
        d("breadcrumbs", "Editor", "Breadcrumbs", "Mostra o caminho do arquivo e dos símbolos acima do editor. Desligado, o editor ocupa o espaço da linha.", Bool),
        // --- end of pending keys ---
        d("appearance", "Aparência", "Modo de aparência", "Fixo usa o tema escolhido; Sistema segue o claro/escuro da área de trabalho.", Choice(&["fixed", "system"])),
        d("theme", "Aparência", "Tema", "Tema de cores usado no modo fixo.", Choice(&[])),
        d("theme_light", "Aparência", "Tema claro", "Tema usado quando o sistema está no modo claro (aparência: sistema).", Choice(&[])),
        d("theme_dark", "Aparência", "Tema escuro", "Tema usado quando o sistema está no modo escuro (aparência: sistema).", Choice(&[])),
        d("theme_chrome", "Aparência", "Cores da interface", "Derivadas mantém o visual calmo e plano de sempre; Do tema usa a barra lateral, as abas e a barra inferior do próprio tema quando o texto continua legível (o resto cai no visual derivado).", Choice(&["derived", "theme"])),
        d("ui_scale", "Aparência", "Escala da interface", "Escala de telas hi-DPI (0,5 a 4). 0 = automático, usa a escala do sistema (também aceita LUMEN_SCALE).", Number { min: 0.0, max: 4.0, step: 0.25 }),
        d("auto_import", "TypeScript", "Importação automática", "Adiciona imports ausentes automaticamente ao aceitar uma sugestão.", Bool),
        d("lsp_narrow", "TypeScript", "Modo enxuto", "Limita o servidor de linguagem ao essencial para gastar menos memória e CPU.", Bool),
        d("code_lens", "TypeScript", "Code lens", "Mostra contagem de referências e implementações acima dos símbolos (custa CPU do servidor).", Bool),
        d("inlay_hints", "TypeScript", "Inlay hints", "Mostra tipos inferidos e nomes de parâmetros em cinza dentro do código (só para as linhas visíveis).", Bool),
        d("tsgo_source", "TypeScript", "Origem do tsgo", "Automático usa o tsgo do projeto se instalado, senão o embutido.", Choice(&["auto", "project", "bundled"])),
        d("git_blame", "Git", "Git blame na linha", "Mostra autor e data do último commit da linha do cursor, em cinza ao fim da linha. Também no comando \"Alternar git blame\".", Bool),
        // Bottom bar: one switch per item. The sidebar toggle (left corner) is mandatory and has none.
        d("status_search", "Barra inferior", "Buscar no projeto", "Botão de busca no projeto (canto esquerdo).", Bool),
        d("status_problems", "Barra inferior", "Problemas", "Contagem de erros e avisos (canto esquerdo).", Bool),
        d("status_position", "Barra inferior", "Linha e coluna", "Posição do cursor (Ln, Col) e quantidade de cursores.", Bool),
        d("status_indent", "Barra inferior", "Indentação", "Espaços ou tabulação do arquivo.", Bool),
        d("status_language", "Barra inferior", "Linguagem", "Linguagem do arquivo ativo.", Bool),
        d("status_terminal", "Barra inferior", "Terminal", "Botão que abre o terminal integrado.", Bool),
        d("status_debug", "Barra inferior", "Depuração", "Botão que abre o painel de depuração.", Bool),
        d("status_git", "Barra inferior", "Git", "Botão do painel Git com a contagem de arquivos alterados.", Bool),
        d("status_ram", "Barra inferior", "Memória", "Uso de memória do editor e dos servidores de linguagem.", Bool),
        // Order of the right-hand items: a stepper per item with its slot (1 = leftmost); - moves it
        // left, + moves it right. Hidden items keep their slot.
        d("status_pos_position", "Barra inferior", "Ordem: Linha e coluna", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_indent", "Barra inferior", "Ordem: Indentação", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_language", "Barra inferior", "Ordem: Linguagem", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_terminal", "Barra inferior", "Ordem: Terminal", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_debug", "Barra inferior", "Ordem: Depuração", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_git", "Barra inferior", "Ordem: Git", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("status_pos_ram", "Barra inferior", "Ordem: Memória", "Posição deste item no grupo da direita (1 = mais à esquerda). − move para a esquerda, + para a direita.", Number { min: 1.0, max: 7.0, step: 1.0 }),
        d("ram_display", "Barra inferior", "Formato da memória", "Longa mostra os números (\"RAM 13 MB + tsgo 65 MB\"); curta mostra só um ícone de chip, com os valores no tooltip.", Choice(&["long", "short"])),
    ]
}

/// Defaults matching `Config::default()` plus the pending keys. Used for the per-row reset icon
/// and as the display value of keys missing from the values map.
pub fn default_values() -> Value {
    json!({
        "format_on_save": false, "auto_import": false, "font_size": 14, "lsp_narrow": true,
        "code_lens": false, "inlay_hints": false, "appearance": "fixed", "theme_chrome": "derived", "theme_light": "github-light-default",
        "theme_dark": "github-dark-default", "tsgo_source": "auto",
        "minimap": true, "minimap_style": "subtle", "word_wrap": false, "sticky_scroll": true, "indent_guides": true, "ui_scale": 0,
        "breadcrumbs": true, "git_blame": true, "ram_display": "long",
        "status_search": true, "status_problems": true, "status_position": true, "status_indent": true, "status_language": true,
        "status_terminal": true, "status_debug": true, "status_git": true, "status_ram": true,
        "status_pos_position": 1, "status_pos_indent": 2, "status_pos_language": 3, "status_pos_terminal": 4,
        "status_pos_debug": 5, "status_pos_git": 6, "status_pos_ram": 7,
    })
}

fn choice_label(s: &str) -> String {
    match s {
        "fixed" => "Fixo".into(),
        "system" => "Sistema".into(),
        "auto" => "Automático".into(),
        "project" => "Projeto".into(),
        "bundled" => "Embutido".into(),
        "long" => "Longa".into(),
        "short" => "Curta".into(),
        "derived" => "Derivadas".into(),
        "theme" => "Do tema".into(),
        "subtle" => "Sutil".into(),
        "normal" => "Normal".into(),
        o => o.to_string(),
    }
}

fn section_icon(s: &str) -> &'static str {
    match s {
        "Geral" => "settings",
        "Editor" => "edit",
        "Aparência" => "palette",
        "TypeScript" => "code",
        "Git" => "branch",
        "Terminal" => "terminal",
        "Barra inferior" => "layout",
        _ => "settings",
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn num_value(v: f64) -> Value {
    let v = (v * 1e6).round() / 1e6;
    if v.fract() == 0.0 && v.abs() < 1e15 { json!(v as i64) } else { json!(v) }
}

/// Segmented control for up to 3 options, dropdown list above that.
fn is_segmented(n: usize) -> bool {
    (1..=3).contains(&n)
}

/// Case-insensitive AND match of every whitespace-separated term of `q` in `hay`.
fn matches_all(hay: &str, q: &str) -> bool {
    let hay = hay.to_lowercase();
    q.to_lowercase().split_whitespace().all(|t| hay.contains(t))
}

const SET_ROW: i32 = 60;
const DROP_VIS: usize = 8;

#[derive(Debug, Clone)]
struct DropState {
    def: usize,
    sel: usize,
    scroll: usize,
}

pub struct SettingsPage {
    pub defs: Vec<SettingDef>,
    values: Map<String, Value>,
    defaults: Map<String, Value>,
    choices: HashMap<String, Vec<String>>,
    pub search: TextField,
    /// Index into [`SettingsPage::sections`].
    pub section: usize,
    /// Index into [`SettingsPage::visible`].
    pub sel: usize,
    pub scroll: f64,
    edit: Option<(usize, TextField)>,
    drop: Option<DropState>,
    view_h: i32,
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
        Self { defs, values, defaults, choices: HashMap::new(), search: TextField::default(), section: 0, sel: 0, scroll: 0.0, edit: None, drop: None, view_h: 380 }
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

    fn searching(&self) -> bool {
        !self.search.text.trim().is_empty()
    }

    /// Indices into `defs` of the rows currently listed: the matches of the search box across
    /// all sections, or every row of the selected section.
    pub fn visible(&self) -> Vec<usize> {
        if self.searching() {
            let q = &self.search.text;
            (0..self.defs.len()).filter(|&i| matches_all(&format!("{} {}", self.defs[i].label, self.defs[i].help), q)).collect()
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

    fn choice_count(&self, def: usize) -> usize {
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
        self.defaults.get(self.defs[def].key).is_some_and(|d| *d != self.current(def))
    }

    fn cur_bool(&self, def: usize) -> bool {
        self.current(def).as_bool().unwrap_or(false)
    }
    fn cur_num(&self, def: usize) -> f64 {
        self.current(def).as_f64().unwrap_or(0.0)
    }
    fn cur_str(&self, def: usize) -> String {
        match self.current(def) {
            Value::String(s) => s,
            Value::Null => String::new(),
            o => o.to_string(),
        }
    }

    fn explicit(&self, def: usize) -> bool {
        self.values.contains_key(self.defs[def].key)
    }

    /// Store `value` and report it, unless the stored value is already equal.
    fn set_value(&mut self, def: usize, value: Value) -> Event {
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

    fn start_edit(&mut self, def: usize) {
        self.drop = None;
        self.edit = Some((def, TextField::with(&self.cur_str(def))));
    }

    fn commit_edit(&mut self) -> Event {
        match self.edit.take() {
            Some((def, f)) => self.set_text(def, &f.text),
            None => Event::None,
        }
    }

    fn open_drop(&mut self, def: usize) {
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

    fn drop_move(&mut self, delta: i64) {
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

    fn max_scroll(&self) -> f64 {
        (self.visible().len() as i32 * SET_ROW - self.view_h).max(0) as f64
    }

    fn ensure_visible(&mut self) {
        let top = (self.sel as i32 * SET_ROW) as f64;
        let bottom = top + SET_ROW as f64;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.view_h as f64 {
            self.scroll = bottom - self.view_h as f64;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn move_sel(&mut self, delta: i64) {
        let n = self.visible().len();
        if n == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
        self.ensure_visible();
    }

    fn page_rows(&self) -> i64 {
        ((self.view_h / SET_ROW).max(1)) as i64
    }

    fn search_changed(&mut self) {
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
                let step = if dy > 0.0 { 1 } else if dy < 0.0 { -1 } else { 0 };
                d.scroll = (d.scroll as i64 + step).clamp(0, max) as usize;
            }
            return;
        }
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
    }

    fn key(&mut self, k: &K, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
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
                if let Some(d) = self.selected_def() {
                    if matches!(self.defs[d].kind, SettingKind::Bool | SettingKind::Choice(_)) {
                        return self.activate(d);
                    }
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

// ───────────────────────────── keybindings model ─────────────────────────────

#[derive(Debug, Clone)]
pub struct CommandRow {
    pub id: String,
    pub label: String,
    /// Normalized default combo, e.g. `ctrl+shift+p`.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Browse,
    /// Waiting for the next key combo for the selected command.
    Record,
    /// The captured combo belongs to `other` (a command id): waiting for "Substituir".
    Confirm { combo: String, other: String },
    /// Waiting for confirmation of "Restaurar padrões".
    ConfirmReset,
}

const KB_ROW: i32 = 34;

pub struct KeybindsPage {
    commands: Vec<CommandRow>,
    defaults: HashMap<String, String>,
    user: HashMap<String, String>,
    pub filter: TextField,
    filtered: Vec<usize>,
    pub sel: usize,
    pub scroll: f64,
    mode: Mode,
    note: Option<String>,
    view_h: i32,
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
            if let Some(r) = rest.strip_prefix(m) {
                if !r.is_empty() {
                    out.push(m.trim_end_matches('+').to_string());
                    rest = r;
                    found = true;
                    break;
                }
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
                "explorer" => "Explorador".into(),
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

fn is_modifier_name(n: &str) -> bool {
    matches!(n, "control" | "shift" | "alt" | "altgraph" | "super" | "meta" | "hyper" | "capslock" | "numlock" | "scrolllock" | "fn" | "fnlock" | "symbol" | "symbollock")
}

fn is_fkey(n: &str) -> bool {
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
        let mut p = Self { commands, defaults, user, filter: TextField::default(), filtered: vec![], sel: 0, scroll: 0.0, mode: Mode::Browse, note: None, view_h: 340 };
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
    fn defaults_of(&self, id: &str) -> Vec<String> {
        let main = self.default_of(id).map(str::to_string);
        let mut v: Vec<String> = self.defaults.iter().filter(|(_, i)| i.as_str() == id).map(|(c, _)| c.clone()).collect();
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

    fn default_of(&self, id: &str) -> Option<&str> {
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

    fn label_of(&self, id: &str) -> String {
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

    /// `(combo, other command id)` while a "Substituir" question is pending.
    pub fn pending_conflict(&self) -> Option<(&str, &str)> {
        match &self.mode {
            Mode::Confirm { combo, other } => Some((combo.as_str(), other.as_str())),
            _ => None,
        }
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    fn refilter(&mut self) {
        let keep = self.selected_id().map(str::to_string);
        let q = self.filter.text.clone();
        let filtered: Vec<usize> = (0..self.commands.len())
            .filter(|&i| {
                let c = &self.commands[i];
                matches_all(&format!("{} {} {}", c.label, c.id, self.bindings_of(&c.id).join(" ")), &q)
            })
            .collect();
        self.filtered = filtered;
        self.sel = keep.and_then(|id| self.filtered.iter().position(|&i| self.commands[i].id == id)).unwrap_or(0).min(self.filtered.len().saturating_sub(1));
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn max_scroll(&self) -> f64 {
        (self.filtered.len() as i32 * KB_ROW - self.view_h).max(0) as f64
    }

    fn ensure_visible(&mut self) {
        let top = (self.sel as i32 * KB_ROW) as f64;
        let bottom = top + KB_ROW as f64;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.view_h as f64 {
            self.scroll = bottom - self.view_h as f64;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn move_sel(&mut self, delta: i64) {
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

    fn remove_binding(&mut self, id: &str, combo: &str) -> Option<Event> {
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

    fn remove_all(&mut self, id: &str) -> Vec<Event> {
        let mut evs = Vec::new();
        for c in self.bindings_of(id) {
            evs.extend(self.remove_binding(id, &c));
        }
        evs
    }

    /// Give `combo` to `id`, dropping the command's other bindings.
    fn apply_bind(&mut self, id: &str, combo: &str) -> Event {
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

    /// Accept the pending "Substituir" / "Restaurar padrões" question.
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
            self.note = Some("Use Ctrl ou Alt junto com a tecla (ou uma tecla F).".into());
            self.mode = Mode::Record;
            return Event::None;
        }
        let combo = keymap::combo(ctrl, shift, alt, &name);
        let Some(id) = self.selected_id().map(str::to_string) else { return Event::None };
        match self.owner_of(&combo).map(str::to_string) {
            Some(o) if o == id => {
                self.mode = Mode::Browse;
                self.note = Some("Esse atalho já está atribuído a este comando.".into());
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

    fn key(&mut self, k: &K, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
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
enum K {
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

fn classify(key: &Key) -> K {
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
fn typed(k: &K, text: Option<&str>, ctrl: bool, alt: bool) -> Option<String> {
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

// ───────────────────────────── modal shell ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings,
    Keybinds,
}

#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Close,
    Tab(Page),
    Section(usize),
    Search,
    SetRow(usize),     // def index
    Toggle(usize),     // def
    Seg(usize, usize), // def, option
    DropOpen(usize),   // def
    DropItem(usize),   // option
    Step(usize, i32),  // def, dir
    EditText(usize),   // def
    SetReset(usize),   // def
    KbRow(usize),      // row k
    KbReset(usize),    // row k
    KbResetAll,
    KbConfirm,
    KbCancel,
}

pub struct Modal {
    pub page: Page,
    pub settings: SettingsPage,
    pub keybinds: KeybindsPage,
    hits: Vec<(Rect, Hit)>,
    rect: Rect,
}

const HEAD_H: i32 = 48;
const SIDE_W: i32 = 176;
const FOOT_H: i32 = 46;

impl Modal {
    pub fn new(settings: SettingsPage, keybinds: KeybindsPage, page: Page) -> Self {
        Self { page, settings, keybinds, hits: vec![], rect: Rect::default() }
    }

    /// Modal rectangle for a window: min(window-80, 860) x min(window-80, 560), centered.
    pub fn rect_for(window: Rect) -> Rect {
        let w = (window.w - 80).min(860).max(120);
        let h = (window.h - 80).min(560).max(120);
        Rect::new(window.x + (window.w - w) / 2, window.y + (window.h - h) / 2, w, h)
    }

    /// Rectangle of the last drawn frame.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn switch_page(&mut self) {
        self.settings.cancel();
        self.keybinds.cancel();
        self.page = if self.page == Page::Settings { Page::Keybinds } else { Page::Settings };
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
        if self.page == Page::Keybinds && self.keybinds.is_capturing() {
            return self.keybinds.record_key(ctrl, shift, alt, key);
        }
        let k = classify(key);
        if ctrl && k == K::Tab {
            self.switch_page();
            return Event::None;
        }
        if k == K::Esc {
            let handled = match self.page {
                Page::Settings => self.settings.cancel(),
                Page::Keybinds => self.keybinds.cancel(),
            };
            return if handled { Event::None } else { Event::Close };
        }
        if ctrl && matches!(&k, K::Ch(ch) if ch.eq_ignore_ascii_case("a")) {
            match self.page {
                Page::Settings => match self.settings.edit.as_mut() {
                    Some((_, f)) => f.select_all(),
                    None => self.settings.search.select_all(),
                },
                Page::Keybinds => self.keybinds.filter.select_all(),
            }
            return Event::None;
        }
        match self.page {
            Page::Settings => self.settings.key(&k, text, ctrl, shift, alt),
            Page::Keybinds => self.keybinds.key(&k, text, ctrl, shift, alt),
        }
    }

    /// Scroll by `dy` pixels (positive = the list scrolls down).
    pub fn wheel(&mut self, dy: f64) {
        match self.page {
            Page::Settings => self.settings.wheel(dy),
            Page::Keybinds => self.keybinds.wheel(dy),
        }
    }

    /// Click at window coordinates; uses the rectangles of the last `draw`.
    pub fn mouse_down(&mut self, x: f64, y: f64) -> Event {
        if !self.rect.contains(x, y) {
            return Event::Close;
        }
        let hit = self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        // An open dropdown swallows any click that is not on one of its items.
        if self.page == Page::Settings && self.settings.dropdown_open() {
            if let Some(Hit::DropItem(o)) = hit {
                let def = self.settings.drop.as_ref().map(|d| d.def);
                self.settings.drop = None;
                return def.map_or(Event::None, |d| self.settings.pick(d, o));
            }
            self.settings.drop = None;
            return Event::None;
        }
        if self.page == Page::Settings && self.settings.is_editing() && !matches!(hit, Some(Hit::EditText(_))) {
            return self.settings.commit_edit();
        }
        if self.page == Page::Keybinds && self.keybinds.is_capturing() && !matches!(hit, Some(Hit::KbConfirm) | Some(Hit::KbCancel)) {
            self.keybinds.cancel();
            if !matches!(hit, Some(Hit::KbRow(_))) {
                return Event::None;
            }
        }
        let Some(hit) = hit else { return Event::None };
        match hit {
            Hit::Close => Event::Close,
            Hit::Tab(p) => {
                if p != self.page {
                    self.switch_page();
                }
                Event::None
            }
            Hit::Search | Hit::DropItem(_) => Event::None,
            Hit::Section(i) => {
                self.settings.search = TextField::default();
                self.settings.set_section(i);
                Event::None
            }
            Hit::SetRow(d) => {
                self.settings.select_def(d);
                Event::None
            }
            Hit::Toggle(d) => {
                self.settings.select_def(d);
                self.settings.toggle(d)
            }
            Hit::Seg(d, o) => {
                self.settings.select_def(d);
                self.settings.pick(d, o)
            }
            Hit::DropOpen(d) => {
                self.settings.select_def(d);
                self.settings.open_drop(d);
                Event::None
            }
            Hit::Step(d, dir) => {
                self.settings.select_def(d);
                self.settings.step(d, dir)
            }
            Hit::EditText(d) => {
                self.settings.select_def(d);
                if !self.settings.is_editing() {
                    self.settings.start_edit(d);
                }
                Event::None
            }
            Hit::SetReset(d) => {
                self.settings.select_def(d);
                self.settings.reset_def(d)
            }
            Hit::KbRow(k) => {
                self.keybinds.select_row(k);
                self.keybinds.begin_record();
                Event::None
            }
            Hit::KbReset(k) => {
                self.keybinds.select_row(k);
                self.keybinds.reset_row(k)
            }
            Hit::KbResetAll => {
                self.keybinds.arm_reset_all();
                Event::None
            }
            Hit::KbConfirm => self.keybinds.confirm(),
            Hit::KbCancel => {
                self.keybinds.cancel();
                Event::None
            }
        }
    }

    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, window: Rect) {
        let c = theme.chrome;
        let fg = theme.foreground;
        let dim = c.tab_inactive_fg;
        let r = Self::rect_for(window);
        self.rect = r;
        self.hits.clear();
        cv.shadow(r, 14);
        cv.card(r, 14, c.widget_bg, c.border);

        // Header: page tabs + close button.
        let mut x = r.x + 18;
        for (page, icon, label) in [(Page::Settings, "settings", "Configurações"), (Page::Keybinds, "keyboard", "Atalhos de teclado")] {
            let active = self.page == page;
            let tw = tx.ui_width(label, active);
            let tab = Rect::new(x, r.y + 9, 16 + 8 + tw + 28, 30);
            if active {
                cv.fill_round(tab, 8, c.list_active_bg);
            }
            tx.icon(cv, &format!("hugeicons/{icon}"), tab.x + 12, tab.y + 7, 16, if active { fg } else { dim }, tab);
            tx.draw_ui(cv, tab.x + 36, tab.y + 6, label, if active { fg } else { dim }, tab, active);
            self.hits.push((tab, Hit::Tab(page)));
            x = tab.right() + 6;
        }
        let close = Rect::new(r.right() - 18 - 28, r.y + 10, 28, 28);
        tx.icon(cv, "hugeicons/close", close.x + 6, close.y + 6, 16, dim, close);
        self.hits.push((close, Hit::Close));
        let hint = "Ctrl+Tab alterna";
        let hw = tx.ui_width(hint, false);
        if close.x - hw - 20 > x {
            tx.draw_ui(cv, close.x - hw - 12, r.y + 15, hint, dim.with_alpha(160), r, false);
        }
        cv.fill(Rect::new(r.x + 1, r.y + HEAD_H, r.w - 2, 1), c.border);

        let body = Rect::new(r.x, r.y + HEAD_H + 1, r.w, r.h - HEAD_H - 1);
        match self.page {
            Page::Settings => draw_settings(&mut self.settings, cv, tx, theme, body, &mut self.hits),
            Page::Keybinds => draw_keybinds(&mut self.keybinds, cv, tx, theme, body, &mut self.hits),
        }
    }
}

// ───────────────────────────── drawing helpers ─────────────────────────────

/// Rounded fill that degrades to a clipped square fill when the shape is cut by `clip`.
fn rr(cv: &mut Canvas, r: Rect, rad: i32, col: Rgba, clip: Rect) {
    if r.intersect(&clip) == r {
        cv.fill_round(r, rad, col);
    } else {
        cv.fill_clip(r, col, clip);
    }
}

fn card_c(cv: &mut Canvas, r: Rect, rad: i32, fill: Rgba, border: Rgba, clip: Rect) {
    if r.intersect(&clip) == r {
        cv.card(r, rad, fill, border);
    } else {
        cv.fill_clip(r, border, clip);
        cv.fill_clip(r.inset(1), fill, clip);
    }
}

fn on_accent(a: Rgba) -> Rgba {
    if a.luminance() > 0.62 { Rgba(20, 20, 20, 255) } else { Rgba(255, 255, 255, 255) }
}

/// `s` shortened with an ellipsis so it fits in `max_w` pixels.
fn fit(tx: &mut TextCtx, s: &str, max_w: i32, bold: bool) -> String {
    if max_w <= 0 {
        return String::new();
    }
    if tx.ui_width(s, bold) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        if tx.ui_width(&cand, bold) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_string() + "…"
}

fn push_hit(hits: &mut Vec<(Rect, Hit)>, r: Rect, clip: Rect, h: Hit) {
    let r = r.intersect(&clip);
    if r.w > 0 && r.h > 0 {
        hits.push((r, h));
    }
}

fn scrollbar(cv: &mut Canvas, area: Rect, scroll: f64, content: i32, col: Rgba) {
    if content <= area.h || area.h <= 0 {
        return;
    }
    let th = ((area.h as f64 * area.h as f64 / content as f64) as i32).max(24).min(area.h);
    let max = (content - area.h) as f64;
    let ty = area.y + (((area.h - th) as f64) * (scroll / max).clamp(0.0, 1.0)) as i32;
    cv.fill_round(Rect::new(area.right() - 5, ty, 4, th), 2, col);
}

fn search_box(cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect, field: &TextField, placeholder: &str, focused: bool) {
    let c = th.chrome;
    card_c(cv, r, 8, c.input_bg, if focused { c.accent } else { c.border }, r);
    tx.icon(cv, "hugeicons/search", r.x + 11, r.y + 9, 16, c.tab_inactive_fg, r);
    let inner = Rect::new(r.x + 34, r.y + 1, r.w - 44, r.h - 2);
    if field.text.is_empty() {
        tx.draw_ui(cv, inner.x, r.y + 8, placeholder, c.tab_inactive_fg, inner, false);
    } else {
        if field.sel_all {
            let w = tx.ui_width(&field.text, false);
            cv.fill_round_clip(Rect::new(inner.x - 3, r.y + 5, w + 6, 24), 5, c.accent.with_alpha(0x55), inner);
        }
        tx.draw_ui(cv, inner.x, r.y + 8, &field.text, th.foreground, inner, false);
    }
    if focused {
        let prefix: String = field.text.chars().take(field.cursor).collect();
        let cx = inner.x + tx.ui_width(&prefix, false);
        cv.fill_clip(Rect::new(cx, r.y + 8, 1, 18), th.foreground, inner);
    }
}

fn button(cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect, label: &str, primary: bool) {
    let c = th.chrome;
    if primary {
        cv.fill_round(r, 8, c.accent);
        let w = tx.ui_width(label, true);
        tx.draw_ui(cv, r.x + (r.w - w) / 2, r.y + (r.h - 18) / 2, label, on_accent(c.accent), r, true);
    } else {
        cv.card(r, 8, c.input_bg, c.border);
        let w = tx.ui_width(label, false);
        tx.draw_ui(cv, r.x + (r.w - w) / 2, r.y + (r.h - 18) / 2, label, th.foreground, r, false);
    }
}

fn draw_chips(tx: &mut TextCtx, cv: &mut Canvas, th: &Theme, x: i32, cy: i32, combos: &[String], max_x: i32, clip: Rect, muted: bool) {
    let c = th.chrome;
    let mut x = x;
    for (ci, combo) in combos.iter().enumerate() {
        if ci > 0 {
            x += 8;
        }
        for part in combo_chips(combo) {
            let tw = tx.ui_width(&part, false);
            let w = (tw + 14).max(22);
            if x + w > max_x {
                return;
            }
            let r = Rect::new(x, cy - 10, w, 20);
            card_c(cv, r, 5, if muted { c.widget_bg } else { c.input_bg }, c.border, clip);
            if !muted {
                cv.fill_clip(Rect::new(r.x + 3, r.bottom() - 2, r.w - 6, 1), c.border, clip);
            }
            tx.draw_ui(cv, r.x + (w - tw) / 2, r.y + 1, &part, if muted { c.tab_inactive_fg } else { th.foreground }, r.intersect(&clip), false);
            x += w + 4;
        }
        x -= 4;
    }
}

// ───────────────────────────── settings drawing ─────────────────────────────

fn draw_settings(p: &mut SettingsPage, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let c = th.chrome;
    let fg = th.foreground;
    let dim = c.tab_inactive_fg;
    let searching = p.searching();

    // Left column: sections.
    cv.fill(Rect::new(area.x + SIDE_W, area.y, 1, area.h - 14), c.border);
    let secs = p.sections();
    let vis = p.visible();
    for (i, s) in secs.iter().enumerate() {
        let r = Rect::new(area.x + 10, area.y + 14 + i as i32 * 36, SIDE_W - 20, 32);
        let active = !searching && i == p.section;
        if active {
            cv.fill_round(r, 8, c.list_active_bg);
        }
        tx.icon(cv, &format!("hugeicons/{}", section_icon(s)), r.x + 10, r.y + 8, 16, if active { fg } else { dim }, r);
        tx.draw_ui(cv, r.x + 36, r.y + 7, s, if active { fg } else { dim }, r, active);
        if searching {
            let n = vis.iter().filter(|&&d| p.defs[d].section == *s).count();
            if n > 0 {
                let t = n.to_string();
                let w = tx.ui_width(&t, false);
                tx.draw_ui(cv, r.right() - w - 10, r.y + 7, &t, c.accent_text, r, false);
            }
        }
        hits.push((r, Hit::Section(i)));
    }

    // Right: search + rows.
    let rx = area.x + SIDE_W + 16;
    let rw = area.w - SIDE_W - 32;
    let sb = Rect::new(rx, area.y + 12, rw, 34);
    search_box(cv, tx, th, sb, &p.search, "Buscar configurações…", p.edit.is_none());
    hits.push((sb, Hit::Search));

    let list = Rect::new(rx - 8, sb.bottom() + 10, rw + 16, (area.bottom() - 30 - (sb.bottom() + 10)).max(0));
    p.view_h = list.h;
    p.scroll = p.scroll.clamp(0.0, p.max_scroll());
    if p.sel >= vis.len() {
        p.sel = vis.len().saturating_sub(1);
    }
    let mut drop_anchor: Option<Rect> = None;
    if vis.is_empty() {
        let msg = "Nenhuma configuração encontrada";
        let w = tx.ui_width(msg, false);
        tx.draw_ui(cv, list.x + (list.w - w) / 2, list.y + 40, msg, dim, list, false);
    }
    for (k, &d) in vis.iter().enumerate() {
        let y = list.y + k as i32 * SET_ROW - p.scroll as i32;
        if y + SET_ROW <= list.y || y >= list.bottom() {
            continue;
        }
        let row = Rect::new(list.x, y, list.w - 10, SET_ROW - 4);
        if k == p.sel {
            rr(cv, row, 10, c.list_active_bg, list);
        }
        push_hit(hits, row, list, Hit::SetRow(d));
        let def = p.defs[d].clone();

        // Reset icon (far right) and control area.
        if p.is_modified(d) {
            let reset = Rect::new(row.right() - 14 - 18, row.y + (row.h - 18) / 2, 18, 18);
            tx.icon(cv, "hugeicons/reset", reset.x + 1, reset.y + 1, 16, dim, list);
            push_hit(hits, reset.inset(-4), list, Hit::SetReset(d));
        }
        let right = row.right() - 14 - 18 - 10;
        let cy = row.y + row.h / 2;
        let ctl_w = match &def.kind {
            SettingKind::Bool => 40,
            SettingKind::Number { .. } => 114,
            SettingKind::Text => 220,
            SettingKind::Choice(_) => {
                let opts = p.choices_of(d);
                if is_segmented(opts.len()) {
                    let mut w = 4;
                    for o in &opts {
                        w += tx.ui_width(&choice_label(o), false) + 22;
                    }
                    w
                } else {
                    let mut m = tx.ui_width(&choice_label(&p.cur_str(d)), false);
                    for o in opts.iter().take(200) {
                        m = m.max(tx.ui_width(&choice_label(o), false));
                    }
                    (m + 48).clamp(130, 230)
                }
            }
        };

        // Label + help.
        let text_w = right - ctl_w - 20 - (row.x + 14);
        let label_s = fit(tx, def.label, text_w, true);
        let lw = tx.ui_width(&label_s, true);
        let clip_row = row.intersect(&list);
        tx.draw_ui(cv, row.x + 14, row.y + 9, &label_s, fg, clip_row, true);
        if searching && lw + 80 < text_w {
            tx.draw_ui(cv, row.x + 14 + lw + 8, row.y + 9, &format!("· {}", def.section), dim.with_alpha(170), clip_row, false);
        }
        let help_s = fit(tx, def.help, text_w, false);
        tx.draw_ui(cv, row.x + 14, row.y + 29, &help_s, dim, clip_row, false);

        // Control.
        match &def.kind {
            SettingKind::Bool => {
                let on = p.cur_bool(d);
                let pill = Rect::new(right - 40, cy - 11, 40, 22);
                rr(cv, pill, 11, if on { c.accent } else { c.border.mix(fg, 0.25) }, list);
                let kx = if on { pill.right() - 19 } else { pill.x + 3 };
                rr(cv, Rect::new(kx, pill.y + 3, 16, 16), 8, if on { on_accent(c.accent) } else { Rgba(250, 250, 250, 255) }, list);
                push_hit(hits, pill, list, Hit::Toggle(d));
            }
            SettingKind::Choice(_) => {
                let opts = p.choices_of(d);
                let cur = p.cur_str(d);
                if is_segmented(opts.len()) {
                    let bar = Rect::new(right - ctl_w, cy - 15, ctl_w, 30);
                    card_c(cv, bar, 8, c.input_bg, c.border, list);
                    let mut x = bar.x + 2;
                    for (oi, o) in opts.iter().enumerate() {
                        let label = choice_label(o);
                        let w = tx.ui_width(&label, false) + 22;
                        let seg = Rect::new(x, bar.y + 2, w, bar.h - 4);
                        let active = *o == cur;
                        if active {
                            rr(cv, seg, 6, c.accent, list);
                        }
                        tx.draw_ui(cv, seg.x + 11, seg.y + 5, &label, if active { on_accent(c.accent) } else { fg }, seg.intersect(&list), active);
                        push_hit(hits, seg, list, Hit::Seg(d, oi));
                        x += w;
                    }
                } else {
                    let btn = Rect::new(right - ctl_w, cy - 15, ctl_w, 30);
                    let open = p.drop.as_ref().is_some_and(|x| x.def == d);
                    card_c(cv, btn, 8, c.input_bg, if open { c.accent } else { c.border }, list);
                    let label = if cur.is_empty() { "—".to_string() } else { choice_label(&cur) };
                    let label = fit(tx, &label, btn.w - 44, false);
                    tx.draw_ui(cv, btn.x + 12, btn.y + 6, &label, fg, btn.intersect(&list), false);
                    tx.icon(cv, "hugeicons/chevron_down", btn.right() - 26, btn.y + 7, 16, dim, btn.intersect(&list));
                    push_hit(hits, btn, list, Hit::DropOpen(d));
                    if open {
                        drop_anchor = Some(btn);
                    }
                }
            }
            SettingKind::Number { min, max, .. } => {
                let v = p.cur_num(d);
                let x0 = right - ctl_w;
                for (i, (dir, icon, enabled)) in [(-1, "minus", v > *min), (1, "plus", v < *max)].into_iter().enumerate() {
                    let bx = if i == 0 { x0 } else { right - 30 };
                    let b = Rect::new(bx, cy - 15, 30, 30);
                    card_c(cv, b, 8, c.input_bg, c.border, list);
                    tx.icon(cv, &format!("hugeicons/{icon}"), b.x + 7, b.y + 7, 16, if enabled { fg } else { dim.with_alpha(90) }, b.intersect(&list));
                    push_hit(hits, b, list, Hit::Step(d, dir));
                }
                let t = fmt_num(v);
                let w = tx.ui_width(&t, true);
                let mid = Rect::new(x0 + 30, cy - 15, ctl_w - 60, 30);
                tx.draw_ui(cv, mid.x + (mid.w - w) / 2, mid.y + 6, &t, fg, mid.intersect(&list), true);
            }
            SettingKind::Text => {
                let b = Rect::new(right - ctl_w, cy - 15, ctl_w, 30);
                let editing = p.edit.as_ref().filter(|(ed, _)| *ed == d);
                card_c(cv, b, 8, c.input_bg, if editing.is_some() { c.accent } else { c.border }, list);
                let inner = Rect::new(b.x + 10, b.y + 1, b.w - 20, b.h - 2).intersect(&list);
                match editing {
                    Some((_, f)) => {
                        if f.sel_all {
                            let w = tx.ui_width(&f.text, false);
                            cv.fill_round_clip(Rect::new(b.x + 7, b.y + 3, w + 6, 24), 5, c.accent.with_alpha(0x55), inner);
                        }
                        tx.draw_ui(cv, b.x + 10, b.y + 6, &f.text, fg, inner, false);
                        let prefix: String = f.text.chars().take(f.cursor).collect();
                        let cx = b.x + 10 + tx.ui_width(&prefix, false);
                        cv.fill_clip(Rect::new(cx, b.y + 6, 1, 18), fg, inner);
                    }
                    None => {
                        let s = p.cur_str(d);
                        if s.is_empty() {
                            tx.draw_ui(cv, b.x + 10, b.y + 6, "—", dim, inner, false);
                        } else {
                            tx.draw_ui(cv, b.x + 10, b.y + 6, &s, fg, inner, false);
                        }
                    }
                }
                push_hit(hits, b, list, Hit::EditText(d));
            }
        }
    }
    scrollbar(cv, list, p.scroll, vis.len() as i32 * SET_ROW, dim.with_alpha(110));

    // Dropdown popup, above everything else.
    if let (Some(a), Some(dr)) = (drop_anchor, p.drop.clone()) {
        let opts = p.choices_of(dr.def);
        let cur = p.cur_str(dr.def);
        let n = opts.len().min(DROP_VIS);
        let ph = n as i32 * 26 + 8;
        let mut maxw = a.w;
        for o in opts.iter().skip(dr.scroll).take(DROP_VIS) {
            maxw = maxw.max(tx.ui_width(&choice_label(o), false) + 40);
        }
        let pw = maxw.min(area.w - SIDE_W - 20);
        let py = if a.bottom() + 4 + ph <= area.bottom() - 6 { a.bottom() + 4 } else { (a.y - 4 - ph).max(area.y + 4) };
        let pr = Rect::new(a.right() - pw, py, pw, ph);
        cv.shadow(pr, 8);
        cv.card(pr, 8, c.widget_bg, c.border);
        hits.push((pr, Hit::Search));
        for (i, o) in opts.iter().enumerate().skip(dr.scroll).take(DROP_VIS) {
            let ir = Rect::new(pr.x + 4, pr.y + 4 + (i - dr.scroll) as i32 * 26, pr.w - 8, 26);
            if i == dr.sel {
                cv.fill_round(ir, 6, c.list_active_bg);
            }
            let label = fit(tx, &choice_label(o), ir.w - 40, false);
            tx.draw_ui(cv, ir.x + 10, ir.y + 4, &label, fg, ir, false);
            if *o == cur {
                tx.icon(cv, "hugeicons/check", ir.right() - 24, ir.y + 5, 16, c.accent, ir);
            }
            hits.push((ir, Hit::DropItem(i)));
        }
        scrollbar(cv, Rect::new(pr.x, pr.y + 4, pr.w - 2, ph - 8), dr.scroll as f64, opts.len() as i32 * 26, dim.with_alpha(120));
    }

    // Footer hint.
    let hint = fit(tx, "↑↓ navegar   Enter/Espaço alterar   ←→ ajustar   Tab trocar de seção   Esc fechar", rw, false);
    tx.draw_ui(cv, rx, area.bottom() - 26, &hint, dim.with_alpha(170), area, false);
}

// ───────────────────────────── keybindings drawing ─────────────────────────────

fn draw_keybinds(p: &mut KeybindsPage, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let c = th.chrome;
    let fg = th.foreground;
    let dim = c.tab_inactive_fg;
    let pad = 20;

    let btn_label = "Restaurar padrões";
    let bw = tx.ui_width(btn_label, false) + 52;
    let btn = Rect::new(area.right() - pad - bw, area.y + 12, bw, 34);
    cv.card(btn, 8, c.input_bg, c.border);
    tx.icon(cv, "hugeicons/reset", btn.x + 12, btn.y + 9, 16, dim, btn);
    tx.draw_ui(cv, btn.x + 36, btn.y + 8, btn_label, fg, btn, false);
    hits.push((btn, Hit::KbResetAll));
    let fr = Rect::new(area.x + pad, area.y + 12, btn.x - 10 - (area.x + pad), 34);
    search_box(cv, tx, th, fr, &p.filter, "Filtrar comandos ou atalhos…", p.mode == Mode::Browse);
    hits.push((fr, Hit::Search));

    // Column captions.
    let hdr_y = fr.bottom() + 8;
    let list = Rect::new(area.x + pad - 8, hdr_y + 24, area.w - 2 * pad + 16, (area.bottom() - FOOT_H - (hdr_y + 24)).max(0));
    let row_x = list.x + 12;
    let tw = list.w - 10 - 24 - 30;
    let (c2, c3) = (row_x + tw * 40 / 100, row_x + tw * 76 / 100);
    for (x, t) in [(row_x, "Comando"), (c2, "Atalho"), (c3, "Padrão")] {
        tx.draw_ui(cv, x, hdr_y + 2, t, dim, area, true);
    }
    cv.fill(Rect::new(area.x + pad - 8, hdr_y + 23, list.w, 1), c.border);

    p.view_h = list.h;
    p.scroll = p.scroll.clamp(0.0, p.max_scroll());
    if p.sel >= p.filtered.len() {
        p.sel = p.filtered.len().saturating_sub(1);
    }
    if p.filtered.is_empty() {
        let msg = "Nenhum comando encontrado";
        let w = tx.ui_width(msg, false);
        tx.draw_ui(cv, list.x + (list.w - w) / 2, list.y + 40, msg, dim, list, false);
    }
    for k in 0..p.filtered.len() {
        let y = list.y + k as i32 * KB_ROW - p.scroll as i32;
        if y + KB_ROW <= list.y || y >= list.bottom() {
            continue;
        }
        let ci = p.filtered[k];
        let cmd = p.commands[ci].clone();
        let row = Rect::new(list.x, y, list.w - 10, KB_ROW - 2);
        let clip = row.intersect(&list);
        let selected = k == p.sel;
        if selected {
            rr(cv, row, 8, c.list_active_bg, list);
        }
        push_hit(hits, row, list, Hit::KbRow(k));
        let cy = row.y + row.h / 2;
        let label = fit(tx, &cmd.label, c2 - row_x - 12, false);
        tx.draw_ui(cv, row_x, cy - 9, &label, fg, clip, false);

        // Current binding(s).
        let recording = selected && p.mode != Mode::Browse && p.mode != Mode::ConfirmReset;
        if recording {
            match &p.mode {
                Mode::Confirm { combo, .. } => draw_chips(tx, cv, th, c2, cy, std::slice::from_ref(combo), c3 - 8, clip, false),
                _ => {
                    let cell = Rect::new(c2 - 6, cy - 13, c3 - c2 - 4, 26);
                    card_c(cv, cell, 6, c.input_bg, c.accent, clip);
                    let t = fit(tx, "Pressione o novo atalho…", cell.w - 16, false);
                    tx.draw_ui(cv, cell.x + 8, cell.y + 4, &t, c.accent_text, cell.intersect(&clip), false);
                }
            }
        } else {
            let cur = p.bindings_of(&cmd.id);
            if cur.is_empty() {
                tx.draw_ui(cv, c2, cy - 9, "—", dim, clip, false);
            } else {
                draw_chips(tx, cv, th, c2, cy, &cur, c3 - 8, clip, false);
            }
        }
        // Default.
        match &cmd.default {
            Some(d) => draw_chips(tx, cv, th, c3, cy, std::slice::from_ref(d), row.right() - 40, clip, true),
            None => tx.draw_ui(cv, c3, cy - 9, "—", dim.with_alpha(120), clip, false),
        }
        if p.is_modified(&cmd.id) {
            let ic = Rect::new(row.right() - 14 - 18, cy - 9, 18, 18);
            tx.icon(cv, "hugeicons/reset", ic.x + 1, ic.y + 1, 16, dim, clip);
            push_hit(hits, ic.inset(-5), list, Hit::KbReset(k));
        }
    }
    scrollbar(cv, list, p.scroll, p.filtered.len() as i32 * KB_ROW, dim.with_alpha(110));

    // Footer: hints, record prompt, conflict question.
    let fy = area.bottom() - FOOT_H;
    cv.fill(Rect::new(area.x + 1, fy, area.w - 2, 1), c.border);
    let ty = fy + (FOOT_H - 18) / 2 - 3;
    let left = area.x + pad;
    let btn_h = 30;
    let btn_y = fy + (FOOT_H - btn_h) / 2 - 3;
    match p.mode.clone() {
        Mode::Browse => {
            let (text, col) = match &p.note {
                Some(m) => (m.clone(), c.warning),
                None => ("Enter gravar atalho   Del remover   Ctrl+R restaurar linha   Ctrl+Shift+R restaurar tudo   Esc fechar".to_string(), dim.with_alpha(170)),
            };
            let text = fit(tx, &text, area.w - 2 * pad, false);
            tx.draw_ui(cv, left, ty, &text, col, area, false);
        }
        Mode::Record => {
            let name = p.selected_id().map(|i| p.label_of(i)).unwrap_or_default();
            let (text, col) = match &p.note {
                Some(m) => (m.clone(), c.warning),
                None => (format!("Pressione o novo atalho para «{name}»…   Esc cancela"), c.accent),
            };
            let text = fit(tx, &text, area.w - 2 * pad, false);
            tx.draw_ui(cv, left, ty, &text, col, area, false);
        }
        Mode::Confirm { combo, other } => {
            let chips = combo_chips(&combo).join("+");
            let text = format!("{chips} já é usado por «{}».", p.label_of(&other));
            let (b1w, b2w) = (96, 90);
            let b2 = Rect::new(area.right() - pad - b2w, btn_y, b2w, btn_h);
            let b1 = Rect::new(b2.x - 8 - b1w, btn_y, b1w, btn_h);
            let text = fit(tx, &text, b1.x - left - 12, false);
            tx.draw_ui(cv, left, ty, &text, c.warning, area, false);
            button(cv, tx, th, b1, "Substituir", true);
            button(cv, tx, th, b2, "Cancelar", false);
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
        Mode::ConfirmReset => {
            let (b1w, b2w) = (96, 90);
            let b2 = Rect::new(area.right() - pad - b2w, btn_y, b2w, btn_h);
            let b1 = Rect::new(b2.x - 8 - b1w, btn_y, b1w, btn_h);
            let text = fit(tx, "Restaurar todos os atalhos para o padrão?", b1.x - left - 12, false);
            tx.draw_ui(cv, left, ty, &text, c.warning, area, false);
            button(cv, tx, th, b1, "Restaurar", true);
            button(cv, tx, th, b2, "Cancelar", false);
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }
    fn named(n: NamedKey) -> Key {
        Key::Named(n)
    }

    fn page_with(values: Value) -> SettingsPage {
        SettingsPage::new(default_settings(), values)
    }

    fn commands() -> Vec<CommandRow> {
        vec![
            CommandRow { id: "QuickOpen".into(), label: "Abrir arquivo".into(), default: Some("ctrl+p".into()) },
            CommandRow { id: "Palette".into(), label: "Paleta de comandos".into(), default: Some("ctrl+shift+p".into()) },
            CommandRow { id: "CloseTab".into(), label: "Fechar aba".into(), default: Some("ctrl+w".into()) },
            CommandRow { id: "TestFile".into(), label: "Testar arquivo".into(), default: None },
            CommandRow { id: "Sidebar".into(), label: "Alternar barra lateral".into(), default: Some("ctrl+b".into()) },
        ]
    }

    fn modal() -> Modal {
        Modal::new(page_with(json!({})), KeybindsPage::new(commands(), HashMap::new()), Page::Settings)
    }

    fn kb() -> KeybindsPage {
        KeybindsPage::new(commands(), HashMap::new())
    }

    /// Apply events to a plain map exactly like the app would, for round-trip checks.
    fn apply(map: &mut HashMap<String, String>, ev: Event) {
        for e in ev.flatten() {
            match e {
                Event::Bind { combo, command_id } => {
                    map.insert(combo, command_id);
                }
                Event::Unbind { combo } => {
                    map.remove(&combo);
                }
                Event::ResetAll => map.clear(),
                _ => {}
            }
        }
    }

    fn press(m: &mut Modal, key: Key) -> Event {
        m.key(&key, None, false, false, false)
    }
    fn typing(m: &mut Modal, s: &str) {
        for c in s.chars() {
            if c == ' ' {
                m.key(&named(NamedKey::Space), Some(" "), false, false, false);
            } else {
                m.key(&ch(&c.to_string()), Some(&c.to_string()), false, false, false);
            }
        }
    }
    fn record(p: &mut KeybindsPage, ctrl: bool, shift: bool, alt: bool, k: &str) -> Event {
        p.record_key(ctrl, shift, alt, &ch(k))
    }
    fn select(p: &mut KeybindsPage, id: &str) {
        p.filter = TextField::default();
        p.refilter();
        let k = p.row_ids().iter().position(|x| *x == id).unwrap();
        p.select_row(k);
    }
    fn def_idx(p: &SettingsPage, key: &str) -> usize {
        p.defs.iter().position(|d| d.key == key).unwrap()
    }
    fn keys_of(p: &SettingsPage) -> Vec<&'static str> {
        p.visible().iter().map(|&i| p.defs[i].key).collect()
    }

    #[test]
    fn default_aliases_count_as_defaults() {
        let rows = vec![
            CommandRow { id: "Redo".into(), label: "Refazer".into(), default: Some("ctrl+shift+z".into()) },
            CommandRow { id: "Save".into(), label: "Salvar".into(), default: Some("ctrl+s".into()) },
        ];
        let mut p = KeybindsPage::new(rows, HashMap::new());
        p.add_default_aliases([("ctrl+y".to_string(), "Redo".to_string()), ("Ctrl+K S".to_string(), "Save".to_string())]);
        assert_eq!(p.bindings_of("Redo"), vec!["ctrl+shift+z", "ctrl+y"], "main default first");
        assert_eq!(p.bindings_of("Save"), vec!["ctrl+s", "ctrl+k s"]);
        assert!(!p.is_modified("Redo"), "an alias is not a modification");
        assert_eq!(p.owner_of("ctrl+y"), Some("Redo"));
        // Removing the alias disables it explicitly and marks the row as modified; reset undoes it.
        let ev = p.remove_binding("Redo", "ctrl+y");
        assert!(matches!(ev, Some(Event::Bind { ref combo, ref command_id }) if combo == "ctrl+y" && command_id == "none"));
        assert!(p.is_modified("Redo"));
        assert_eq!(p.to_json(), r#"{"ctrl+y":"none"}"#);
        p.reset_row(0);
        assert!(!p.is_modified("Redo"));
        assert_eq!(p.to_json(), "{}");
    }

    // --- defaults / model ---

    #[test]
    fn default_settings_have_unique_real_keys() {
        let defs = default_settings();
        let mut keys: Vec<_> = defs.iter().map(|d| d.key).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n);
        for k in ["theme", "appearance", "font_size", "format_on_save", "auto_import", "lsp_narrow", "code_lens", "tsgo_source", "theme_light", "theme_dark"] {
            assert!(defs.iter().any(|d| d.key == k), "{k}");
        }
        for k in PENDING_KEYS {
            assert!(defs.iter().any(|d| d.key == *k), "{k}");
        }
        assert!(matches!(defs.iter().find(|d| d.key == "font_size").unwrap().kind, SettingKind::Number { min, max, .. } if min == 8.0 && max == 40.0));
    }

    #[test]
    fn bottom_bar_has_one_toggle_per_optional_item_and_none_for_the_mandatory_one() {
        let defs = default_settings();
        let vals = default_values();
        for (id, _, required) in crate::config::STATUS_ITEMS {
            let key = format!("status_{id}");
            let def = defs.iter().find(|d| d.key == key);
            if *required {
                assert!(def.is_none(), "{id} is mandatory: no switch");
            } else {
                let def = def.unwrap_or_else(|| panic!("missing switch {key}"));
                assert_eq!(def.section, "Barra inferior");
                assert_eq!(def.kind, SettingKind::Bool);
                assert_eq!(vals[&key], json!(true), "{key} defaults to visible");
            }
        }
        for k in ["breadcrumbs", "git_blame", "ram_display"] {
            assert!(defs.iter().any(|d| d.key == k), "{k}");
            assert!(vals.get(k).is_some(), "{k} default");
        }
        assert!(matches!(&defs.iter().find(|d| d.key == "ram_display").unwrap().kind, SettingKind::Choice(o) if *o == ["long", "short"]));
    }

    #[test]
    fn bottom_bar_order_has_one_stepper_per_right_hand_item() {
        let defs = default_settings();
        let vals = default_values();
        assert_eq!(crate::config::STATUS_RIGHT.len(), 7);
        for (slot, id) in crate::config::STATUS_RIGHT.iter().enumerate() {
            let key = format!("status_pos_{id}");
            let def = defs.iter().find(|d| d.key == key).unwrap_or_else(|| panic!("missing {key}"));
            assert!(matches!(def.kind, SettingKind::Number { min, max, step } if min == 1.0 && max == 7.0 && step == 1.0));
            assert_eq!(vals[&key], json!(slot + 1), "{key} default slot");
        }
    }

    #[test]
    fn set_values_replaces_what_the_rows_show() {
        let mut p = page_with(json!({"status_pos_git": 6}));
        let def = p.defs.iter().position(|d| d.key == "status_pos_git").unwrap();
        assert_eq!(p.current(def), json!(6));
        p.set_values(json!({"status_pos_git": 2}));
        assert_eq!(p.current(def), json!(2));
    }

    #[test]
    fn sections_in_order_and_visible_by_section() {
        let mut p = page_with(json!({}));
        assert_eq!(p.sections(), vec!["Geral", "Editor", "Aparência", "TypeScript", "Git", "Barra inferior"]);
        assert_eq!(p.visible().len(), 1);
        p.set_section(3);
        assert_eq!(keys_of(&p), vec!["auto_import", "lsp_narrow", "code_lens", "inlay_hints", "tsgo_source"]);
        p.set_section(99);
        assert_eq!(p.section, 3, "out of range ignored");
    }

    #[test]
    fn search_filters_label_and_help_case_insensitive_across_sections() {
        let mut m = modal();
        typing(&mut m, "MINIMAPA");
        assert_eq!(keys_of(&m.settings), vec!["minimap", "minimap_style"]);
        m.settings.search = TextField::with("servidor");
        let keys = keys_of(&m.settings);
        assert!(keys.contains(&"format_on_save") && keys.contains(&"lsp_narrow") && keys.contains(&"code_lens"), "{keys:?}");
        m.settings.search = TextField::with("zzzz");
        assert!(m.settings.visible().is_empty());
        assert_eq!(m.settings.selected_def(), None);
        // multiple terms are ANDed (label or help)
        m.settings.search = TextField::with("tema escuro");
        let keys = keys_of(&m.settings);
        assert!(keys.contains(&"theme_dark") && !keys.contains(&"font_size"), "{keys:?}");
        m.settings.search = TextField::with("   ");
        assert_eq!(keys_of(&m.settings), vec!["format_on_save"], "blank search = section list");
    }

    #[test]
    fn typing_resets_selection_and_backspace_restores_list() {
        let mut m = modal();
        typing(&mut m, "tema");
        assert!(m.settings.visible().len() >= 3);
        press(&mut m, named(NamedKey::ArrowDown));
        assert_eq!(m.settings.sel, 1);
        typing(&mut m, "x");
        assert_eq!(m.settings.sel, 0);
        for _ in 0..5 {
            press(&mut m, named(NamedKey::Backspace));
        }
        assert!(m.settings.search.text.is_empty());
        assert_eq!(m.settings.visible().len(), 1, "back to the section list");
    }

    #[test]
    fn bool_toggle_emits_events_via_enter_space_and_arrows() {
        let mut m = modal();
        let ev = press(&mut m, named(NamedKey::Enter));
        assert_eq!(ev, Event::Setting { key: "format_on_save".into(), value: json!(true) });
        let ev = press(&mut m, named(NamedKey::Space));
        assert_eq!(ev, Event::Setting { key: "format_on_save".into(), value: json!(false) });
        assert_eq!(press(&mut m, named(NamedKey::ArrowRight)), Event::Setting { key: "format_on_save".into(), value: json!(true) });
        assert_eq!(press(&mut m, named(NamedKey::ArrowRight)), Event::None, "already on");
        assert_eq!(press(&mut m, named(NamedKey::ArrowLeft)), Event::Setting { key: "format_on_save".into(), value: json!(false) });
        assert_eq!(m.settings.values()["format_on_save"], json!(false));
    }

    #[test]
    fn space_goes_to_search_when_search_is_not_empty() {
        let mut m = modal();
        typing(&mut m, "formatar ao");
        assert_eq!(m.settings.search.text, "formatar ao");
        assert_eq!(m.settings.values().get("format_on_save"), None, "nothing toggled");
    }

    #[test]
    fn choice_cycles_forward_and_backward_with_wrap() {
        let mut p = page_with(json!({"appearance": "fixed"}));
        let d = def_idx(&p, "appearance");
        assert_eq!(p.cycle(d, 1), Event::Setting { key: "appearance".into(), value: json!("system") });
        assert_eq!(p.cycle(d, 1), Event::Setting { key: "appearance".into(), value: json!("fixed") });
        assert_eq!(p.cycle(d, -1), Event::Setting { key: "appearance".into(), value: json!("system") });
        let t = def_idx(&p, "tsgo_source");
        assert_eq!(p.cycle(t, 1), Event::Setting { key: "tsgo_source".into(), value: json!("project") }, "auto -> project");
        assert_eq!(p.pick(t, 2), Event::Setting { key: "tsgo_source".into(), value: json!("bundled") });
        assert_eq!(p.cycle(t, 1), Event::Setting { key: "tsgo_source".into(), value: json!("auto") }, "wraps");
        assert_eq!(p.pick(t, 0), Event::None, "same value");
        assert_eq!(p.pick(t, 9), Event::None);
    }

    #[test]
    fn dynamic_choices_and_dropdown_pick() {
        let mut m = modal();
        let themes: Vec<String> = (0..20).map(|i| format!("theme-{i}")).collect();
        m.settings.set_choices("theme", themes.clone());
        m.settings.set_section(2);
        let d = def_idx(&m.settings, "theme");
        assert_eq!(m.settings.choices_of(d), themes);
        press(&mut m, named(NamedKey::ArrowDown));
        assert_eq!(m.settings.selected_def(), Some(d));
        assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
        assert!(m.settings.dropdown_open());
        press(&mut m, named(NamedKey::ArrowDown));
        press(&mut m, named(NamedKey::ArrowDown));
        let ev = press(&mut m, named(NamedKey::Enter));
        assert_eq!(ev, Event::Setting { key: "theme".into(), value: json!("theme-2") }, "list starts at 0 without a current value");
        assert!(!m.settings.dropdown_open());
        // Esc closes the dropdown only, a second Esc closes the modal.
        press(&mut m, named(NamedKey::Enter));
        assert!(m.settings.dropdown_open());
        assert_eq!(m.settings.drop.as_ref().unwrap().sel, 2, "opens on the current value");
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
        assert!(!m.settings.dropdown_open());
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::Close);
    }

    #[test]
    fn dropdown_selection_is_clamped_and_scrolls() {
        let mut m = modal();
        m.settings.set_choices("theme", (0..30).map(|i| i.to_string()).collect());
        m.settings.set_section(2);
        press(&mut m, named(NamedKey::ArrowDown));
        press(&mut m, named(NamedKey::Enter));
        for _ in 0..50 {
            press(&mut m, named(NamedKey::ArrowDown));
        }
        let d = m.settings.drop.clone().unwrap();
        assert_eq!(d.sel, 29);
        assert_eq!(d.scroll, 30 - DROP_VIS);
        press(&mut m, named(NamedKey::Home));
        let d = m.settings.drop.clone().unwrap();
        assert_eq!((d.sel, d.scroll), (0, 0));
        m.wheel(40.0);
        assert_eq!(m.settings.drop.as_ref().unwrap().scroll, 1);
    }

    #[test]
    fn number_step_clamps_to_range() {
        let mut p = page_with(json!({"font_size": 39}));
        let d = def_idx(&p, "font_size");
        assert_eq!(p.step(d, 1), Event::Setting { key: "font_size".into(), value: json!(40) });
        assert_eq!(p.step(d, 1), Event::None, "already at max");
        assert_eq!(p.set_number(d, 8.0), Event::Setting { key: "font_size".into(), value: json!(8) });
        assert_eq!(p.step(d, -1), Event::None, "already at min");
        assert_eq!(p.set_number(d, 1000.0), Event::Setting { key: "font_size".into(), value: json!(40) });
        assert_eq!(p.set_number(d, -5.0), Event::Setting { key: "font_size".into(), value: json!(8) });
    }

    #[test]
    fn number_out_of_range_value_is_pulled_in_and_fractions_survive() {
        let mut p = SettingsPage::new(
            vec![SettingDef { key: "scale", section: "Geral", label: "Escala", help: "", kind: SettingKind::Number { min: 0.5, max: 2.0, step: 0.25 } }],
            json!({"scale": 9}),
        );
        assert_eq!(p.step(0, -1), Event::Setting { key: "scale".into(), value: json!(2) });
        assert_eq!(p.step(0, -1), Event::Setting { key: "scale".into(), value: json!(1.75) });
        assert_eq!(p.cur_num(0), 1.75);
        assert_eq!(fmt_num(1.75), "1.75");
        assert_eq!(fmt_num(14.0), "14");
    }

    #[test]
    fn number_via_keyboard_arrows() {
        let mut m = modal();
        m.settings.set_section(1);
        assert_eq!(m.settings.selected_def(), Some(def_idx(&m.settings, "font_size")));
        assert_eq!(press(&mut m, named(NamedKey::ArrowRight)), Event::Setting { key: "font_size".into(), value: json!(15) });
        assert_eq!(press(&mut m, named(NamedKey::ArrowLeft)), Event::Setting { key: "font_size".into(), value: json!(14) });
        assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
    }

    #[test]
    fn text_setting_edit_commit_and_cancel() {
        let defs = vec![SettingDef { key: "shell", section: "Geral", label: "Shell", help: "Programa do terminal", kind: SettingKind::Text }];
        let mut m = Modal::new(SettingsPage::new(defs, json!({"shell": "bash"})), KeybindsPage::new(vec![], HashMap::new()), Page::Settings);
        press(&mut m, named(NamedKey::Enter));
        assert!(m.settings.is_editing());
        typing(&mut m, "ç");
        assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::Setting { key: "shell".into(), value: json!("bashç") });
        assert!(!m.settings.is_editing());
        // cancel keeps the value
        press(&mut m, named(NamedKey::Enter));
        typing(&mut m, "x");
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
        assert!(!m.settings.is_editing());
        assert_eq!(m.settings.values()["shell"], json!("bashç"));
        // committing an unchanged text emits nothing
        press(&mut m, named(NamedKey::Enter));
        assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
    }

    #[test]
    fn reset_row_restores_default_and_flags_modified() {
        let mut p = page_with(json!({"lsp_narrow": false, "font_size": 14}));
        let d = def_idx(&p, "lsp_narrow");
        assert!(p.is_modified(d));
        assert!(!p.is_modified(def_idx(&p, "font_size")));
        assert_eq!(p.reset_def(d), Event::Setting { key: "lsp_narrow".into(), value: json!(true) });
        assert!(!p.is_modified(d));
        assert_eq!(p.reset_def(d), Event::None);
    }

    #[test]
    fn settings_navigation_bounds_and_tab() {
        let mut m = modal();
        m.settings.set_section(3); // 5 rows
        press(&mut m, named(NamedKey::ArrowUp));
        assert_eq!(m.settings.sel, 0);
        for _ in 0..10 {
            press(&mut m, named(NamedKey::ArrowDown));
        }
        assert_eq!(m.settings.sel, 4);
        press(&mut m, named(NamedKey::Home));
        assert_eq!(m.settings.sel, 0);
        press(&mut m, named(NamedKey::End));
        assert_eq!(m.settings.sel, 4);
        press(&mut m, named(NamedKey::PageUp));
        assert_eq!(m.settings.sel, 0);
        press(&mut m, named(NamedKey::PageDown));
        assert_eq!(m.settings.sel, 4);
        // Tab walks sections and wraps
        let last = m.settings.sections().len() - 1;
        m.settings.set_section(last);
        press(&mut m, named(NamedKey::Tab));
        assert_eq!((m.settings.section, m.settings.sel), (0, 0));
        m.key(&named(NamedKey::Tab), None, false, true, false);
        assert_eq!(m.settings.section, last);
    }

    #[test]
    fn selection_keeps_row_in_view_and_wheel_clamps() {
        let defs: Vec<SettingDef> = (0..30)
            .map(|i| SettingDef { key: Box::leak(format!("k{i}").into_boxed_str()), section: "Geral", label: "Item", help: "", kind: SettingKind::Bool })
            .collect();
        let mut p = SettingsPage::new(defs, json!({}));
        p.view_h = 240;
        for _ in 0..10 {
            p.move_sel(1);
        }
        assert_eq!(p.sel, 10);
        assert_eq!(p.scroll, (11 * SET_ROW - 240) as f64);
        p.wheel(10_000.0);
        assert_eq!(p.scroll, (30 * SET_ROW - 240) as f64, "wheel clamps at the end");
        p.wheel(-10_000.0);
        assert_eq!(p.scroll, 0.0);
    }

    // --- keybindings ---

    #[test]
    fn combo_chips_and_parts() {
        assert_eq!(combo_chips("ctrl+shift+p"), vec!["Ctrl", "Shift", "P"]);
        assert_eq!(combo_chips("alt+left"), vec!["Alt", "←"]);
        assert_eq!(combo_chips("f5"), vec!["F5"]);
        assert_eq!(combo_chips("ctrl++"), vec!["Ctrl", "+"]);
        assert_eq!(combo_chips("ctrl+pageup"), vec!["Ctrl", "PgUp"]);
        assert_eq!(combo_parts("ctrl+shift+alt+x"), vec!["ctrl", "shift", "alt", "x"]);
        assert_eq!(combo_chips("ctrl+k r"), vec!["Ctrl", "K", "›", "R"]);
        assert_eq!(combo_chips("explorer:shift+a"), vec!["Explorador", "Shift", "A"]);
        assert_eq!(combo_chips("explorer:delete"), vec!["Explorador", "Del"]);
    }

    #[test]
    fn keybinds_filter_by_label_id_and_combo() {
        let mut p = kb();
        assert_eq!(p.rows().len(), 5);
        p.filter = TextField::with("ABRIR");
        p.refilter();
        assert_eq!(p.row_ids(), vec!["QuickOpen"]);
        p.filter = TextField::with("closetab");
        p.refilter();
        assert_eq!(p.row_ids(), vec!["CloseTab"]);
        p.filter = TextField::with("ctrl shift");
        p.refilter();
        assert_eq!(p.row_ids(), vec!["Palette"]);
        p.filter = TextField::with("nada");
        p.refilter();
        assert!(p.rows().is_empty() && p.selected().is_none());
    }

    #[test]
    fn record_free_combo_binds_and_replaces_default() {
        let mut p = kb();
        select(&mut p, "QuickOpen");
        p.begin_record();
        assert!(p.is_recording());
        let ev = record(&mut p, true, false, false, "K");
        assert_eq!(
            ev,
            Event::Batch(vec![
                Event::Bind { combo: "ctrl+p".into(), command_id: "none".into() },
                Event::Bind { combo: "ctrl+k".into(), command_id: "QuickOpen".into() },
            ])
        );
        assert!(!p.is_capturing());
        assert_eq!(p.bindings_of("QuickOpen"), vec!["ctrl+k"]);
        assert_eq!(p.owner_of("ctrl+p"), None);
        assert!(p.is_modified("QuickOpen"));
    }

    #[test]
    fn record_unbound_command_gets_single_bind_event() {
        let mut p = kb();
        select(&mut p, "TestFile");
        p.begin_record();
        let ev = p.record_key(true, true, false, &ch("T"));
        assert_eq!(ev, Event::Bind { combo: "ctrl+shift+t".into(), command_id: "TestFile".into() });
        assert_eq!(p.to_json(), r#"{"ctrl+shift+t":"TestFile"}"#);
    }

    #[test]
    fn record_rejects_bare_keys_ignores_modifiers_and_escape_cancels() {
        let mut p = kb();
        select(&mut p, "TestFile");
        p.begin_record();
        assert_eq!(record(&mut p, false, false, false, "a"), Event::None);
        assert_eq!(record(&mut p, false, true, false, "a"), Event::None);
        assert!(p.is_recording() && p.note().is_some(), "still recording with a hint");
        assert_eq!(p.record_key(true, false, false, &named(NamedKey::Control)), Event::None);
        assert_eq!(p.record_key(false, true, false, &named(NamedKey::Shift)), Event::None);
        assert!(p.is_recording());
        assert!(p.user().is_empty());
        // F-keys are fine without modifiers
        let ev = p.record_key(false, false, false, &named(NamedKey::F5));
        assert_eq!(ev, Event::Bind { combo: "f5".into(), command_id: "TestFile".into() });
        // Esc cancels
        p.begin_record();
        assert_eq!(p.record_key(false, false, false, &named(NamedKey::Escape)), Event::None);
        assert!(!p.is_capturing());
        assert_eq!(p.bindings_of("TestFile"), vec!["f5"], "unchanged");
    }

    #[test]
    fn conflict_requires_confirmation_then_replaces() {
        let mut p = kb();
        select(&mut p, "TestFile");
        p.begin_record();
        // ctrl+w belongs to CloseTab by default
        assert_eq!(record(&mut p, true, false, false, "w"), Event::None);
        assert_eq!(p.pending_conflict(), Some(("ctrl+w", "CloseTab")));
        assert!(p.is_capturing());
        assert!(p.user().is_empty(), "nothing applied yet");
        let ev = p.confirm();
        assert_eq!(ev, Event::Bind { combo: "ctrl+w".into(), command_id: "TestFile".into() });
        assert_eq!(p.owner_of("ctrl+w"), Some("TestFile"));
        assert!(p.bindings_of("CloseTab").is_empty());
        assert!(!p.is_capturing());
    }

    #[test]
    fn conflict_with_user_binding_cancel_and_enter_confirm() {
        let mut user = HashMap::new();
        user.insert("ctrl+shift+t".to_string(), "TestFile".to_string());
        let mut p = KeybindsPage::new(commands(), user);
        select(&mut p, "Sidebar");
        p.begin_record();
        assert_eq!(p.record_key(true, true, false, &ch("t")), Event::None);
        assert_eq!(p.pending_conflict(), Some(("ctrl+shift+t", "TestFile")));
        // Esc cancels the question without changes
        assert_eq!(p.record_key(false, false, false, &named(NamedKey::Escape)), Event::None);
        assert!(!p.is_capturing());
        assert_eq!(p.owner_of("ctrl+shift+t"), Some("TestFile"));
        // again, confirm with Enter: Sidebar leaves ctrl+b (masked) and takes ctrl+shift+t
        p.begin_record();
        p.record_key(true, true, false, &ch("t"));
        let ev = p.record_key(false, false, false, &named(NamedKey::Enter));
        assert_eq!(
            ev,
            Event::Batch(vec![
                Event::Bind { combo: "ctrl+b".into(), command_id: "none".into() },
                Event::Bind { combo: "ctrl+shift+t".into(), command_id: "Sidebar".into() },
            ])
        );
        assert!(p.bindings_of("TestFile").is_empty());
    }

    #[test]
    fn rebinding_to_own_combo_is_a_noop() {
        let mut p = kb();
        select(&mut p, "CloseTab");
        p.begin_record();
        assert_eq!(record(&mut p, true, false, false, "w"), Event::None);
        assert!(!p.is_capturing() && p.pending_conflict().is_none());
        assert!(p.user().is_empty());
    }

    #[test]
    fn delete_unbinds_default_with_none_and_user_binding_with_unbind() {
        let mut p = kb();
        select(&mut p, "CloseTab");
        assert_eq!(p.unbind_selected(), Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() });
        assert_eq!(p.to_json(), r#"{"ctrl+w":"none"}"#);
        assert_eq!(p.unbind_selected(), Event::None, "nothing left to remove");
        select(&mut p, "TestFile");
        p.begin_record();
        record(&mut p, true, true, false, "t");
        assert_eq!(p.unbind_selected(), Event::Unbind { combo: "ctrl+shift+t".into() });
        assert_eq!(p.to_json(), r#"{"ctrl+w":"none"}"#);
    }

    #[test]
    fn delete_and_backspace_keys_through_modal() {
        let mut m = modal();
        m.switch_page();
        press(&mut m, named(NamedKey::ArrowDown));
        press(&mut m, named(NamedKey::ArrowDown));
        assert_eq!(m.keybinds.selected_id(), Some("CloseTab"));
        assert_eq!(press(&mut m, named(NamedKey::Delete)), Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() });
        // With text in the filter, Backspace edits the filter instead.
        typing(&mut m, "a");
        assert_eq!(press(&mut m, named(NamedKey::Backspace)), Event::None);
        assert!(m.keybinds.filter.text.is_empty());
        // With an empty filter, Backspace removes bindings of the selected row (CloseTab has none now).
        assert_eq!(press(&mut m, named(NamedKey::Backspace)), Event::None);
        press(&mut m, named(NamedKey::ArrowUp));
        assert_eq!(m.keybinds.selected_id(), Some("Palette"));
        assert_eq!(press(&mut m, named(NamedKey::Backspace)), Event::Bind { combo: "ctrl+shift+p".into(), command_id: "none".into() });
    }

    #[test]
    fn reset_row_restores_default_and_drops_user_entries() {
        let mut p = kb();
        select(&mut p, "QuickOpen");
        p.begin_record();
        record(&mut p, true, false, false, "k");
        assert!(p.is_modified("QuickOpen"));
        let k = p.row_ids().iter().position(|x| *x == "QuickOpen").unwrap();
        let ev = p.reset_row(k);
        assert_eq!(ev.flatten().len(), 2, "unbind ctrl+k and ctrl+p none");
        assert!(!p.is_modified("QuickOpen"));
        assert_eq!(p.bindings_of("QuickOpen"), vec!["ctrl+p"]);
        assert_eq!(p.to_json(), "{}");
        assert_eq!(p.reset_row(k), Event::None);
    }

    #[test]
    fn reset_all_clears_user_map_and_needs_confirmation_via_keyboard() {
        let mut m = modal();
        m.switch_page();
        press(&mut m, named(NamedKey::Delete)); // QuickOpen: ctrl+p -> none
        assert!(!m.keybinds.user().is_empty());
        m.key(&ch("r"), None, true, true, false); // Ctrl+Shift+R arms
        assert!(m.keybinds.is_capturing());
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None, "Esc cancels the question");
        assert!(!m.keybinds.is_capturing());
        assert!(!m.keybinds.user().is_empty());
        m.key(&ch("r"), None, true, true, false);
        assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::ResetAll);
        assert!(m.keybinds.user().is_empty());
        assert_eq!(m.keybinds.to_json(), "{}");
        assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+p"]);
    }

    #[test]
    fn to_json_is_minimal_sorted_and_round_trips_through_keymap() {
        let mut user = HashMap::new();
        user.insert("Ctrl+Shift+T".to_string(), "TestFile".to_string());
        user.insert("ctrl+w".to_string(), "none".to_string());
        user.insert("ctrl+p".to_string(), "QuickOpen".to_string()); // equals default -> dropped
        user.insert("ctrl+9".to_string(), "none".to_string()); // none without a default -> dropped
        let p = KeybindsPage::new(commands(), user);
        let js = p.to_json();
        assert_eq!(js, r#"{"ctrl+shift+t":"TestFile","ctrl+w":"none"}"#);
        let km = keymap::Keymap::from_json(&js);
        assert_eq!(km.len(), 2);
        assert_eq!(km.lookup(true, true, false, &ch("t")), Some("TestFile"));
        assert_eq!(km.lookup(true, false, false, &ch("w")), Some("none"));
        assert_eq!(km.lookup(true, false, false, &ch("p")), Some("QuickOpen")); // not in the file: the built-in default answers
        // and feeding it back into a page gives the same document
        let p2 = KeybindsPage::new(commands(), serde_json::from_str::<HashMap<String, String>>(&js).unwrap());
        assert_eq!(p2.to_json(), js);
    }

    #[test]
    fn events_replayed_on_a_plain_map_match_the_page_state() {
        let mut p = kb();
        let mut map: HashMap<String, String> = HashMap::new();
        select(&mut p, "QuickOpen");
        p.begin_record();
        let ev = record(&mut p, true, false, false, "k");
        apply(&mut map, ev);
        select(&mut p, "TestFile");
        p.begin_record();
        record(&mut p, true, false, false, "b"); // conflict with Sidebar
        let ev = p.confirm();
        apply(&mut map, ev);
        select(&mut p, "CloseTab");
        let ev = p.unbind_selected();
        apply(&mut map, ev);
        assert_eq!(&map, p.user());
        select(&mut p, "QuickOpen");
        let k = p.sel;
        let ev = p.reset_row(k);
        apply(&mut map, ev);
        assert_eq!(&map, p.user());
        let ev = p.reset_all();
        apply(&mut map, ev);
        assert!(map.is_empty() && p.user().is_empty());
    }

    #[test]
    fn keybinds_navigation_bounds_and_enter_starts_recording() {
        let mut m = modal();
        m.switch_page();
        press(&mut m, named(NamedKey::ArrowUp));
        assert_eq!(m.keybinds.sel, 0);
        for _ in 0..20 {
            press(&mut m, named(NamedKey::ArrowDown));
        }
        assert_eq!(m.keybinds.sel, 4);
        press(&mut m, named(NamedKey::Home));
        assert_eq!(m.keybinds.sel, 0);
        press(&mut m, named(NamedKey::End));
        assert_eq!(m.keybinds.sel, 4);
        press(&mut m, named(NamedKey::PageUp));
        assert_eq!(m.keybinds.sel, 0);
        press(&mut m, named(NamedKey::Enter));
        assert!(m.keybinds.is_recording());
        // while recording, keys are captured by the page, not navigated
        let ev = m.key(&ch("j"), Some("j"), true, false, false);
        assert!(matches!(ev, Event::Batch(_)), "{ev:?}");
        assert!(!m.keybinds.is_recording());
        assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+j"]);
    }

    // --- modal shell ---

    #[test]
    fn ctrl_tab_switches_pages_and_esc_cancels_recording_before_closing() {
        let mut m = modal();
        assert_eq!(m.page, Page::Settings);
        assert_eq!(m.key(&named(NamedKey::Tab), None, true, false, false), Event::None);
        assert_eq!(m.page, Page::Keybinds);
        m.key(&named(NamedKey::Tab), None, true, false, false);
        assert_eq!(m.page, Page::Settings);
        m.switch_page();
        press(&mut m, named(NamedKey::Enter));
        assert!(m.keybinds.is_recording());
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
        assert!(!m.keybinds.is_recording());
        assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::Close);
        // Ctrl+Tab is a valid combo while recording (captured, not a page switch)
        press(&mut m, named(NamedKey::Enter));
        m.key(&named(NamedKey::Tab), None, true, false, false);
        assert_eq!(m.page, Page::Keybinds);
        assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+tab"]);
    }

    #[test]
    fn rect_is_centered_and_sized() {
        let r = Modal::rect_for(Rect::new(0, 0, 1920, 1080));
        assert_eq!((r.w, r.h), (860, 560));
        assert_eq!((r.x, r.y), ((1920 - 860) / 2, (1080 - 560) / 2));
        let r = Modal::rect_for(Rect::new(0, 0, 600, 400));
        assert_eq!((r.w, r.h), (520, 320));
        let r = Modal::rect_for(Rect::new(10, 20, 600, 400));
        assert_eq!((r.x, r.y), (10 + 40, 20 + 40));
    }

    #[test]
    fn event_flatten() {
        let e = Event::Batch(vec![Event::None, Event::Batch(vec![Event::ResetAll]), Event::Close]);
        assert_eq!(e.flatten(), vec![Event::ResetAll, Event::Close]);
        assert!(Event::None.flatten().is_empty());
    }

    // --- drawing (needs fonts of the machine, not the GPU) ---

    fn test_theme() -> Theme {
        Theme::load(&crate::config::assets_dir().join("themes/dark-plus.json")).unwrap()
    }

    const BG: u32 = 0x010203;

    fn center(r: Rect) -> (f64, f64) {
        ((r.x + r.w / 2) as f64, (r.y + r.h / 2) as f64)
    }

    fn hit_rect(m: &Modal, h: Hit) -> Rect {
        m.hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap_or_else(|| panic!("no hit {h:?}"))
    }

    #[test]
    fn draw_smoke_stays_inside_modal_and_hits_work() {
        let (w, h) = (1000i32, 700i32);
        let mut px = vec![BG; (w * h) as usize];
        let theme = test_theme();
        let mut tx = TextCtx::new(None);
        let window = Rect::new(0, 0, w, h);
        let mut m = modal();
        m.settings.set_choices("theme", vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        {
            let mut cv = Canvas { px: &mut px, w, h };
            m.draw(&mut cv, &mut tx, &theme, window);
        }
        let r = m.rect();
        assert_eq!(r, Modal::rect_for(window));
        let near = r.inset(-26); // the shadow reaches ~24px
        let mut inside_changed = 0i64;
        for y in 0..h {
            for x in 0..w {
                let v = px[(y * w + x) as usize];
                if r.contains(x as f64, y as f64) {
                    inside_changed += (v != BG) as i64;
                } else if !near.contains(x as f64, y as f64) {
                    assert_eq!(v, BG, "({x},{y}) outside the modal+shadow must be untouched");
                }
            }
        }
        assert!(inside_changed > (r.w as i64 * r.h as i64) / 2, "modal body painted");
        // Tabs are clickable: click the second tab, then draw the keybinds page.
        let tab = hit_rect(&m, Hit::Tab(Page::Keybinds));
        assert_eq!(m.mouse_down((tab.x + 5) as f64, (tab.y + 5) as f64), Event::None);
        assert_eq!(m.page, Page::Keybinds);
        {
            let mut cv = Canvas { px: &mut px, w, h };
            m.draw(&mut cv, &mut tx, &theme, window);
        }
        // Clicking a row starts recording; a conflicting combo shows the confirm buttons.
        let row = hit_rect(&m, Hit::KbRow(2));
        m.mouse_down((row.x + 20) as f64, (row.y + 5) as f64);
        assert!(m.keybinds.is_recording());
        assert_eq!(m.keybinds.selected_id(), Some("CloseTab"));
        assert_eq!(m.key(&ch("p"), None, true, false, false), Event::None);
        assert!(m.keybinds.pending_conflict().is_some());
        {
            let mut cv = Canvas { px: &mut px, w, h };
            m.draw(&mut cv, &mut tx, &theme, window);
        }
        let (x, y) = center(hit_rect(&m, Hit::KbConfirm));
        let ev = m.mouse_down(x, y);
        assert_eq!(
            ev,
            Event::Batch(vec![
                Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() },
                Event::Bind { combo: "ctrl+p".into(), command_id: "CloseTab".into() },
            ])
        );
        // Outside click closes.
        assert_eq!(m.mouse_down(1.0, 1.0), Event::Close);
    }

    #[test]
    fn settings_clicks_toggle_step_and_dropdown_via_hits() {
        let (w, h) = (1000i32, 700i32);
        let mut px = vec![BG; (w * h) as usize];
        let theme = test_theme();
        let mut tx = TextCtx::new(None);
        let window = Rect::new(0, 0, w, h);
        let mut m = modal();
        let mut redraw = |m: &mut Modal| {
            let mut cv = Canvas { px: &mut px, w, h };
            m.draw(&mut cv, &mut tx, &theme, window);
        };
        redraw(&mut m);
        // Geral: toggle format_on_save
        let d = def_idx(&m.settings, "format_on_save");
        let (x, y) = center(hit_rect(&m, Hit::Toggle(d)));
        assert_eq!(m.mouse_down(x, y), Event::Setting { key: "format_on_save".into(), value: json!(true) });
        // Editor section: "+" on font size
        let (x, y) = center(hit_rect(&m, Hit::Section(1)));
        m.mouse_down(x, y);
        redraw(&mut m);
        let d = def_idx(&m.settings, "font_size");
        let (x, y) = center(hit_rect(&m, Hit::Step(d, 1)));
        assert_eq!(m.mouse_down(x, y), Event::Setting { key: "font_size".into(), value: json!(15) });
        // Aparência: segmented option click, then dropdown open + pick
        m.settings.set_choices("theme", vec!["x".into(), "y".into(), "z".into(), "w".into(), "v".into()]);
        let (x, y) = center(hit_rect(&m, Hit::Section(2)));
        m.mouse_down(x, y);
        redraw(&mut m);
        let d = def_idx(&m.settings, "appearance");
        let (x, y) = center(hit_rect(&m, Hit::Seg(d, 1)));
        assert_eq!(m.mouse_down(x, y), Event::Setting { key: "appearance".into(), value: json!("system") });
        let d = def_idx(&m.settings, "theme");
        let (x, y) = center(hit_rect(&m, Hit::DropOpen(d)));
        assert_eq!(m.mouse_down(x, y), Event::None);
        assert!(m.settings.dropdown_open());
        redraw(&mut m);
        let (x, y) = center(hit_rect(&m, Hit::DropItem(3)));
        assert_eq!(m.mouse_down(x, y), Event::Setting { key: "theme".into(), value: json!("w") });
        assert!(!m.settings.dropdown_open());
    }
}
