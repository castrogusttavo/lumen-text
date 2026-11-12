//! VS Code theme loader (the JSON format used by Shiki / tm-themes).
//!
//! Scope resolution follows TextMate rules: the rule with the most specific
//! selector wins, ties go to the later rule in the file.

use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// `theme_chrome = "theme"` in config.json: sidebar, tabs and status bar take the theme's own
/// colors wherever they pass the contrast rules (see `Theme::load_mode`). Process-wide so that
/// every `Theme::load` (startup, theme picker, system appearance) follows the setting.
static THEME_CHROME: AtomicBool = AtomicBool::new(false);

/// Choose between the derived minimal chrome (`false`, the default) and the theme's own.
pub fn set_theme_chrome(own: bool) {
    THEME_CHROME.store(own, Ordering::Relaxed);
}

pub fn theme_chrome() -> bool {
    THEME_CHROME.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    /// WCAG contrast ratio between two opaque colors.
    pub fn contrast(self, o: Rgba) -> f32 {
        let l = |c: Rgba| {
            let f = |v: u8| {
                let s = v as f32 / 255.0;
                if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.0) + 0.7152 * f(c.1) + 0.0722 * f(c.2)
        };
        let (a, b) = (l(self), l(o));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Move `self` toward black or white (whichever helps) until it has `min` contrast on every
    /// background in `on`.
    pub fn readable_on(self, on: &[Rgba], min: f32) -> Rgba {
        let dark_bg = on.iter().map(|b| b.luminance()).sum::<f32>() / on.len() as f32 > 0.5;
        let target = if dark_bg { Rgba(0, 0, 0, 255) } else { Rgba(255, 255, 255, 255) };
        let mut c = self;
        for step in 0..=20 {
            c = self.mix(target, step as f32 * 0.05);
            if on.iter().all(|b| c.contrast(*b) >= min) {
                break;
            }
        }
        c
    }
    pub fn parse(hex: &str) -> Option<Self> {
        let h = hex.strip_prefix('#')?;
        let p = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            6 => Some(Self(p(0)?, p(2)?, p(4)?, 255)),
            8 => Some(Self(p(0)?, p(2)?, p(4)?, p(6)?)),
            3 => {
                let n = |i: usize| u8::from_str_radix(&h[i..=i], 16).ok().map(|v| v * 17);
                Some(Self(n(0)?, n(1)?, n(2)?, 255))
            }
            _ => None,
        }
    }
}

/// Colors for the UI around the text (sidebar, tabs, status bar, widgets, diagnostics, git).
#[derive(Debug, Clone, Copy)]
pub struct Chrome {
    pub sidebar_bg: Rgba,
    pub sidebar_fg: Rgba,
    pub tabbar_bg: Rgba,
    pub tab_active_bg: Rgba,
    pub tab_inactive_bg: Rgba,
    pub tab_active_fg: Rgba,
    pub tab_inactive_fg: Rgba,
    pub status_bg: Rgba,
    pub status_fg: Rgba,
    pub border: Rgba,
    pub widget_bg: Rgba,
    pub list_active_bg: Rgba,
    pub list_hover_bg: Rgba,
    pub input_bg: Rgba,
    pub accent: Rgba,
    /// `accent` nudged until it reads (>= 4.6:1) on the editor and on selected rows: for text.
    pub accent_text: Rgba,
    /// Text/icon color for things drawn on a solid `accent` fill (primary buttons, check marks): >= 4.5:1.
    pub on_accent: Rgba,
    pub error: Rgba,
    pub warning: Rgba,
    pub info: Rgba,
    pub git_added: Rgba,
    pub git_modified: Rgba,
    pub git_deleted: Rgba,
    pub find_match: Rgba,
    pub find_match_current: Rgba,
    /// Diff row / changed-word backgrounds (translucent): the editor text reads (>= 4.5:1) on all four.
    pub diff_add: Rgba,
    pub diff_del: Rgba,
    pub diff_add_word: Rgba,
    pub diff_del_word: Rgba,
    /// Vertical rulers (`editorRuler.foreground`).
    pub ruler: Rgba,
    /// Document highlights: read / write occurrences of the symbol under the caret
    /// (`editor.wordHighlightBackground` / `editor.wordHighlightStrongBackground`, translucent;
    /// the editor text reads on both).
    pub word_hl: Rgba,
    pub word_hl_strong: Rgba,
}

impl Rgba {
    pub fn luminance(self) -> f32 {
        (0.299 * self.0 as f32 + 0.587 * self.1 as f32 + 0.114 * self.2 as f32) / 255.0
    }

    /// Move toward white (positive) or black (negative) by `amt` (0..255).
    pub fn shift(self, amt: i16) -> Rgba {
        let f = |v: u8| (v as i16 + amt).clamp(0, 255) as u8;
        Rgba(f(self.0), f(self.1), f(self.2), self.3)
    }

    /// Linear blend toward `o` by `t` (0..1), opaque.
    pub fn mix(self, o: Rgba, t: f32) -> Rgba {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgba(f(self.0, o.0), f(self.1, o.1), f(self.2, o.2), 255)
    }

    pub fn with_alpha(self, a: u8) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }

    /// `self` (with its own alpha) painted over the opaque color `bg`.
    pub fn over(self, bg: Rgba) -> Rgba {
        bg.mix(self, self.3 as f32 / 255.0)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    pub fg: Option<Rgba>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Scope {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize, Default)]
struct RawSettings {
    foreground: Option<String>,
    #[serde(rename = "fontStyle")]
    font_style: Option<String>,
}

#[derive(Deserialize)]
struct RawRule {
    scope: Option<Scope>,
    #[serde(default)]
    settings: RawSettings,
}

#[derive(Deserialize)]
struct RawTheme {
    name: Option<String>,
    #[serde(default)]
    colors: HashMap<String, serde_json::Value>,
    #[serde(rename = "tokenColors", default)]
    token_colors: Vec<RawRule>,
}

struct Rule {
    /// Selector split into scope prefixes, e.g. "meta.function entity.name" -> 2 parts.
    selector: Vec<String>,
    style: Style,
}

pub struct Theme {
    pub name: String,
    pub background: Rgba,
    pub foreground: Rgba,
    pub selection: Rgba,
    pub cursor: Rgba,
    pub line_highlight: Rgba,
    pub line_number: Rgba,
    pub line_number_active: Rgba,
    /// Dots and arrows of `render_whitespace` (`editorWhitespace.foreground`, else a faded foreground).
    pub whitespace: Rgba,
    pub chrome: Chrome,
    rules: Vec<Rule>,
}

impl Theme {
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with(path, true)
    }

    /// `fix = false` keeps the theme's own colors as authored (used by the accessibility report to
    /// show the "before" numbers); the app always loads with `fix = true`.
    pub fn load_with(path: &Path, fix: bool) -> Result<Self, String> {
        Self::load_mode(path, fix, theme_chrome())
    }

    /// [`load_with`](Self::load_with) with the chrome mode given explicitly: `own_chrome = true`
    /// lets the sidebar, tab bar, active tab and status bar keep the theme's own background (and
    /// the theme's text colors for them) when every UI text color still reads on it (>= 4.5:1,
    /// the same pairs the accessibility test checks). Each surface and each text color falls back
    /// on its own to the derived minimal chrome when it would fail. Only meaningful with `fix`.
    pub fn load_mode(path: &Path, fix: bool, own_chrome: bool) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let raw: RawTheme = serde_json::from_str(&text).map_err(|e| e.to_string())?;

        let color = |key: &str, fallback: Rgba| {
            raw.colors.get(key).and_then(|c| c.as_str()).and_then(Rgba::parse).unwrap_or(fallback)
        };
        let background = color("editor.background", Rgba(0x1e, 0x1e, 0x1e, 255));
        let foreground = color("editor.foreground", Rgba(0xd4, 0xd4, 0xd4, 255));

        let selection_raw = color("editor.selectionBackground", Rgba(0x26, 0x4f, 0x78, 0xff));
        let cursor = color("editorCursor.foreground", foreground);

        let dark = background.luminance() < 0.5;
        let ck = |key: &str, fallback: Rgba| color(key, fallback);
        // Minimal chrome: every surface is the editor background plus a whisper of the
        // foreground (same idea as `color-mix(fg 6%, transparent)`), never the theme's own
        // sidebar/tab colors, so all themes share one calm, flat look.
        let tint = |t: f32| background.mix(foreground, t);
        // Calm editor: a whisper of a current-line band and dim line numbers on every theme.
        let line_highlight = tint(if dark { 0.05 } else { 0.04 });
        let accent_raw = ck("focusBorder", Rgba(0x00, 0x7a, 0xcc, 0xff));
        // The accent is a non-text UI part (check boxes, underlines, rings): WCAG asks 3:1 on the editor.
        let accent_raw = if fix && accent_raw.over(background).contrast(background) < 3.0 {
            accent_raw.over(background).readable_on(&[background], 3.0)
        } else {
            accent_raw
        };
        // Surfaces text can sit on. Every UI text color is nudged (toward black/white, keeping its hue)
        // until it reads (>= 4.5:1, WCAG AA) on all of them: editor, sidebar, tabs, widgets, inputs and
        // the selected / hovered row. The tests in `mod tests` (accessibility) guard it.
        let surfaces = [
            background,
            tint(if dark { 0.025 } else { 0.022 }),
            tint(if dark { 0.075 } else { 0.06 }),
            tint(if dark { 0.045 } else { 0.02 }),
            tint(if dark { 0.12 } else { 0.09 }),
            tint(if dark { 0.10 } else { 0.075 }),
            tint(if dark { 0.06 } else { 0.04 }),
            line_highlight,
        ];
        let ok = |c: Rgba, min: f32| {
            if !fix || surfaces.iter().all(|b| c.over(*b).contrast(*b) >= min) {
                c
            } else {
                c.readable_on(&surfaces, min)
            }
        };
        // Translucent highlight (selection, search match) under `fg`: weaken it until `fg` reads on it.
        let tame = |c: Rgba, base: Rgba, fg: Rgba, floor: u8| {
            let mut c = c;
            while fix && c.3 > floor && fg.contrast(c.over(base)) < 4.5 {
                c.3 = c.3.saturating_sub(8).max(floor);
            }
            c
        };
        let fg_ui = ok(foreground, 4.5);
        // `readable_on` blends toward black/white without alpha; bake the translucency in first.
        let soft = |a: u8| ok(background.mix(foreground, a as f32 / 255.0), 4.5);
        let line_number =
            if fix { ok(tint(if dark { 0.38 } else { 0.42 }), 4.5) } else { tint(if dark { 0.38 } else { 0.42 }) };
        let line_number_active =
            if fix { ok(tint(if dark { 0.78 } else { 0.8 }), 4.5) } else { tint(if dark { 0.78 } else { 0.8 }) };
        let selection = tame(selection_raw, background, fg_ui, 0x30);
        let find_match =
            tame(ck("editor.findMatchHighlightBackground", Rgba(0xea, 0x5c, 0x00, 0x55)), background, fg_ui, 0x08);
        let find_match_current =
            tame(ck("editor.findMatchBackground", Rgba(0x51, 0xc0, 0xa8, 0x80)), background, fg_ui, 0x08);
        let foreground = fg_ui;
        let whitespace =
            color("editorWhitespace.foreground", background.mix(foreground, if dark { 0.24 } else { 0.28 }));
        let sem_raw = |key: &str, fb: Rgba| ok(ck(key, fb), 4.5);
        let (git_added, git_deleted) = (
            sem_raw("editorGutter.addedBackground", Rgba(0x58, 0xa6, 0x4a, 0xff)),
            sem_raw("editorGutter.deletedBackground", Rgba(0xf8, 0x51, 0x49, 0xff)),
        );
        let diff_add = tame(git_added.with_alpha(0x26), background, fg_ui, 0x00);
        let diff_del = tame(git_deleted.with_alpha(0x26), background, fg_ui, 0x00);
        let diff_add_word = tame(git_added.with_alpha(0x55), background, fg_ui, 0x00);
        let diff_del_word = tame(git_deleted.with_alpha(0x55), background, fg_ui, 0x00);
        let ruler = ck("editorRuler.foreground", tint(if dark { 0.16 } else { 0.14 }));
        let (wh, whs) = if dark {
            (Rgba(0x57, 0x57, 0x57, 0xb8), Rgba(0x00, 0x49, 0x72, 0xb8))
        } else {
            (Rgba(0x57, 0x57, 0x57, 0x40), Rgba(0x0e, 0x63, 0x9c, 0x40))
        };
        let word_hl = tame(ck("editor.wordHighlightBackground", wh), background, fg_ui, 0x00);
        let word_hl_strong = tame(ck("editor.wordHighlightStrongBackground", whs), background, fg_ui, 0x00);
        // Text on the accent fill: white or near-black, whichever reads better; a mid-tone accent that
        // reads on neither is pushed away from the editor background until one of them passes.
        let (accent_raw, on_accent) = {
            let (w, k) = (Rgba(255, 255, 255, 255), Rgba(0x10, 0x10, 0x10, 255));
            let best = if w.contrast(accent_raw) >= k.contrast(accent_raw) { w } else { k };
            if !fix || best.contrast(accent_raw) >= 4.5 {
                (accent_raw, best)
            } else {
                let txt = if dark { k } else { w };
                (accent_raw.readable_on(&[txt], 4.5), txt)
            }
        };
        let accent_text = ok(accent_raw, 4.6);
        let sem = |key: &str, fb: Rgba| ok(ck(key, fb), 4.5);
        let mut chrome = Chrome {
            sidebar_bg: surfaces[1],
            sidebar_fg: soft(0xd0),
            tabbar_bg: background,
            tab_active_bg: surfaces[2],
            tab_inactive_bg: background,
            tab_active_fg: foreground,
            tab_inactive_fg: soft(0xb4),
            status_bg: background,
            status_fg: soft(0xb4),
            border: tint(if dark { 0.10 } else { 0.085 }),
            widget_bg: surfaces[3],
            list_active_bg: surfaces[4],
            list_hover_bg: surfaces[5],
            input_bg: surfaces[6],
            accent: accent_raw,
            accent_text,
            on_accent,
            error: sem("editorError.foreground", Rgba(0xf4, 0x47, 0x47, 0xff)),
            warning: sem("editorWarning.foreground", Rgba(0xcc, 0xa7, 0x00, 0xff)),
            info: sem("editorInfo.foreground", Rgba(0x37, 0x94, 0xff, 0xff)),
            git_added,
            git_modified: sem("editorGutter.modifiedBackground", Rgba(0x1b, 0x81, 0xa8, 0xff)),
            git_deleted,
            find_match,
            find_match_current,
            diff_add,
            diff_del,
            diff_add_word,
            diff_del_word,
            ruler,
            word_hl,
            word_hl_strong,
        };
        if own_chrome && fix {
            let opt = |keys: &[&str]| {
                keys.iter().find_map(|k| raw.colors.get(*k).and_then(|c| c.as_str()).and_then(Rgba::parse))
            };
            apply_theme_chrome(&mut chrome, background, line_highlight, foreground, opt);
        }

        let mut rules = Vec::new();
        for r in &raw.token_colors {
            let Some(scope) = &r.scope else { continue };
            let selectors: Vec<&str> = match scope {
                Scope::One(s) => s.split(',').collect(),
                Scope::Many(v) => v.iter().flat_map(|s| s.split(',')).collect(),
            };
            let fs = r.settings.font_style.as_deref().unwrap_or("");
            // Syntax colors under 4.5:1 on the editor (comments are the usual offenders) are nudged
            // toward black/white, keeping their hue, just enough to pass WCAG AA.
            let tok_fg = r.settings.foreground.as_deref().and_then(Rgba::parse).map(|c| {
                if fix && c.over(background).contrast(background) < 4.5 {
                    c.over(background).readable_on(&[background, line_highlight], 4.5)
                } else {
                    c
                }
            });
            let style = Style {
                fg: tok_fg,
                bold: fs.contains("bold"),
                italic: fs.contains("italic"),
                underline: fs.contains("underline"),
            };
            for sel in selectors {
                let parts: Vec<String> = sel.split_whitespace().map(str::to_string).collect();
                if !parts.is_empty() {
                    rules.push(Rule { selector: parts, style });
                }
            }
        }

        Ok(Self {
            name: raw.name.unwrap_or_default(),
            background,
            foreground,
            selection,
            cursor,
            line_highlight,
            line_number,
            line_number_active,
            whitespace,
            chrome,
            rules,
        })
    }

    /// Resolve a style for a scope stack (outermost first), e.g.
    /// ["source.ts", "meta.function.ts", "entity.name.function.ts"].
    pub fn resolve(&self, stack: &[&str]) -> Style {
        let mut best: Option<(usize, usize, &Rule)> = None; // (depth matched, specificity)
        for rule in &self.rules {
            if let Some((depth, spec)) = match_selector(&rule.selector, stack) {
                let better = match best {
                    None => true,
                    Some((d, s, _)) => (depth, spec) >= (d, s),
                };
                if better {
                    best = Some((depth, spec, rule));
                }
            }
        }
        let mut out = Style::default();
        if let Some((_, _, r)) = best {
            out = r.style;
        }
        if out.fg.is_none() {
            out.fg = Some(self.foreground);
        }
        out
    }
}

/// Does `fg` (its own alpha painted over `bg`) read on `bg`?
fn reads(fg: Rgba, bg: Rgba, min: f32) -> bool {
    fg.over(bg).contrast(bg) + 0.005 >= min
}

/// Swap the derived chrome surfaces and chrome text colors for the theme's own where they pass.
/// `opt` looks a VS Code color key up (first key present wins).
fn apply_theme_chrome(
    c: &mut Chrome,
    background: Rgba,
    line_highlight: Rgba,
    foreground: Rgba,
    opt: impl Fn(&[&str]) -> Option<Rgba>,
) {
    // Every UI text color has to keep reading on a surface for it to be adopted: the derived text
    // colors are left alone (the editor keeps its look), the surface is what gives way.
    let texts = |c: &Chrome| {
        [
            c.tab_inactive_fg,
            c.tab_active_fg,
            c.status_fg,
            c.sidebar_fg,
            c.accent_text,
            foreground,
            c.error,
            c.warning,
            c.info,
            c.git_added,
            c.git_modified,
            c.git_deleted,
        ]
    };
    let fits = |c: &Chrome, bg: Rgba| texts(c).iter().all(|f| reads(*f, bg, 4.5));
    let surface = |keys: &[&str]| opt(keys).map(|s| s.over(background));
    if let Some(s) = surface(&["sideBar.background"]).filter(|s| fits(c, *s)) {
        c.sidebar_bg = s;
    }
    if let Some(s) = surface(&["tab.activeBackground"]).filter(|s| fits(c, *s)) {
        c.tab_active_bg = s;
    }
    if let Some(s) = surface(&["editorGroupHeader.tabsBackground", "tab.inactiveBackground"]).filter(|s| fits(c, *s)) {
        c.tabbar_bg = s;
        c.tab_inactive_bg = s;
    }
    if let Some(s) = surface(&["statusBar.background"]).filter(|s| fits(c, *s)) {
        c.status_bg = s;
    }
    // The theme's own text colors, only when they read on every surface the app can put them on.
    let surfaces = [
        background,
        c.sidebar_bg,
        c.tab_active_bg,
        c.widget_bg,
        c.list_active_bg,
        c.list_hover_bg,
        c.input_bg,
        line_highlight,
        c.tabbar_bg,
        c.tab_inactive_bg,
        c.status_bg,
    ];
    let text = |keys: &[&str], cur: Rgba| {
        opt(keys).map(|t| t.over(background)).filter(|t| surfaces.iter().all(|s| reads(*t, *s, 4.5))).unwrap_or(cur)
    };
    c.sidebar_fg = text(&["sideBar.foreground"], c.sidebar_fg);
    c.tab_active_fg = text(&["tab.activeForeground"], c.tab_active_fg);
    c.tab_inactive_fg = text(&["tab.inactiveForeground"], c.tab_inactive_fg);
    c.status_fg = text(&["statusBar.foreground"], c.status_fg);
}

fn is_prefix(selector: &str, scope: &str) -> bool {
    scope == selector || (scope.starts_with(selector) && scope.as_bytes().get(selector.len()) == Some(&b'.'))
}

/// Match a descendant selector against the stack. Returns
/// (index of the deepest scope matched by the last part, total prefix length).
fn match_selector(selector: &[String], stack: &[&str]) -> Option<(usize, usize)> {
    let last = selector.last()?;
    let mut spec = last.len();
    let mut idx = stack.iter().rposition(|s| is_prefix(last, s))?;
    let depth = idx;
    for part in selector[..selector.len() - 1].iter().rev() {
        idx = stack[..idx].iter().rposition(|s| is_prefix(part, s))?;
        spec += part.len();
    }
    Some((depth, spec))
}

#[cfg(test)]
impl Chrome {
    fn test_default() -> Chrome {
        let c = Rgba(0, 0, 0, 255);
        Chrome {
            sidebar_bg: c,
            sidebar_fg: c,
            tabbar_bg: c,
            tab_active_bg: c,
            tab_inactive_bg: c,
            tab_active_fg: c,
            tab_inactive_fg: c,
            status_bg: c,
            status_fg: c,
            border: c,
            widget_bg: c,
            list_active_bg: c,
            list_hover_bg: c,
            input_bg: c,
            accent: c,
            accent_text: c,
            on_accent: c,
            error: c,
            warning: c,
            info: c,
            git_added: c,
            git_modified: c,
            git_deleted: c,
            find_match: c,
            find_match_current: c,
            diff_add: c,
            diff_del: c,
            diff_add_word: c,
            diff_del_word: c,
            ruler: c,
            word_hl: c,
            word_hl_strong: c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(rules: &[(&str, &str)]) -> Theme {
        Theme {
            name: "t".into(),
            background: Rgba(0, 0, 0, 255),
            foreground: Rgba(1, 1, 1, 255),
            selection: Rgba(0, 0, 0, 255),
            cursor: Rgba(0, 0, 0, 255),
            line_highlight: Rgba(0, 0, 0, 255),
            line_number: Rgba(0, 0, 0, 255),
            line_number_active: Rgba(0, 0, 0, 255),
            whitespace: Rgba(0, 0, 0, 255),
            chrome: Chrome::test_default(),
            rules: rules
                .iter()
                .map(|(sel, c)| Rule {
                    selector: sel.split_whitespace().map(str::to_string).collect(),
                    style: Style { fg: Rgba::parse(c), ..Default::default() },
                })
                .collect(),
        }
    }

    #[test]
    fn more_specific_scope_wins() {
        let t = theme(&[("entity", "#111111"), ("entity.name.function", "#222222")]);
        let s = t.resolve(&["source.ts", "entity.name.function.ts"]);
        assert_eq!(s.fg, Rgba::parse("#222222"));
    }

    #[test]
    fn prefix_requires_dot_boundary() {
        let t = theme(&[("string", "#333333")]);
        let s = t.resolve(&["source.ts", "stringy.thing"]);
        assert_eq!(s.fg, Some(Rgba(1, 1, 1, 255))); // fallback to foreground
    }

    #[test]
    fn parent_selector_must_match() {
        let t = theme(&[("meta.function entity.name", "#444444")]);
        assert!(t.resolve(&["entity.name.function"]).fg == Some(Rgba(1, 1, 1, 255)));
        assert_eq!(t.resolve(&["meta.function.ts", "entity.name.function"]).fg, Rgba::parse("#444444"));
    }

    #[test]
    fn every_bundled_theme_loads() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                Theme::load(&p).unwrap_or_else(|err| panic!("{}: {err}", p.display()));
                n += 1;
            }
        }
        assert!(n > 50, "found only {n} themes");
    }

    fn bundled_with(fix: bool) -> Vec<(String, Theme)> {
        bundled_mode(fix, false)
    }

    fn bundled_mode(fix: bool, own_chrome: bool) -> Vec<(String, Theme)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes");
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| {
                (p.file_stem().unwrap().to_string_lossy().into_owned(), Theme::load_mode(&p, fix, own_chrome).unwrap())
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Every text/background pair the UI draws, as (label, contrast ratio, minimum required).
    /// WCAG 2.x AA: 4.5:1 for text; 3:1 for non-text UI parts (the accent fill).
    fn audit(t: &Theme) -> Vec<(String, f32, f32)> {
        let mut out = Vec::new();
        let c = t.chrome;
        let mut pair = |label: String, fg: Rgba, bg: Rgba, min: f32| out.push((label, fg.over(bg).contrast(bg), min));
        // Chrome text on every surface it can sit on.
        let bgs = [
            ("background", t.background),
            ("sidebar_bg", c.sidebar_bg),
            ("tab_active_bg", c.tab_active_bg),
            ("widget_bg", c.widget_bg),
            ("list_active_bg", c.list_active_bg),
            ("list_hover_bg", c.list_hover_bg),
            ("input_bg", c.input_bg),
            ("line_highlight", t.line_highlight),
            ("tabbar_bg", c.tabbar_bg),
            ("tab_inactive_bg", c.tab_inactive_bg),
            ("status_bg", c.status_bg),
        ];
        let fgs = [
            ("tab_inactive_fg", c.tab_inactive_fg),
            ("tab_active_fg", c.tab_active_fg),
            ("status_fg", c.status_fg),
            ("sidebar_fg", c.sidebar_fg),
            ("accent_text", c.accent_text),
            ("foreground", t.foreground),
            ("error", c.error),
            ("warning", c.warning),
            ("info", c.info),
            ("git_added", c.git_added),
            ("git_modified", c.git_modified),
            ("git_deleted", c.git_deleted),
        ];
        for (bn, bg) in bgs {
            for (fname, fg) in fgs {
                pair(format!("{fname} on {bn}"), fg, bg, 4.5);
            }
        }
        // Editor gutter.
        for (bn, bg) in [("background", t.background), ("line_highlight", t.line_highlight)] {
            pair(format!("line_number on {bn}"), t.line_number, bg, 4.5);
            pair(format!("line_number_active on {bn}"), t.line_number_active, bg, 4.5);
        }
        // Text under translucent highlights: selection, search matches, diff additions/removals.
        for (n, hl) in [
            ("selection", t.selection),
            ("find_match", c.find_match),
            ("find_match_current", c.find_match_current),
            ("diff removed", c.diff_del),
            ("diff added", c.diff_add),
            ("diff removed (word)", c.diff_del_word),
            ("diff added (word)", c.diff_add_word),
            ("word highlight", c.word_hl),
            ("word highlight (write)", c.word_hl_strong),
        ] {
            pair(format!("foreground on {n}"), t.foreground, hl.over(t.background), 4.5);
        }
        // Matched letters of the pickers sit on the selected / hovered row.
        pair("accent_text on picker row (selected)".into(), c.accent_text, c.list_active_bg, 4.5);
        pair("accent_text on picker row (hover)".into(), c.accent_text, c.list_hover_bg, 4.5);
        // Primary button / check mark on the accent fill, and the fill itself as a UI part.
        pair("on_accent on accent".into(), c.on_accent, c.accent, 4.5);
        pair("accent (UI part) on background".into(), c.accent, t.background, 3.0);
        // Terminal / output: ANSI palette after `tasks::legible`.
        for (i, col) in crate::tasks::PALETTE.iter().enumerate() {
            pair(format!("ansi[{i}] on background"), crate::tasks::legible(*col, t.background), t.background, 4.5);
        }
        // Syntax tokens on the editor.
        for r in &t.rules {
            if let Some(fg) = r.style.fg {
                pair(format!("token `{}`", r.selector.join(" ")), fg, t.background, 4.5);
            }
        }
        out
    }

    #[test]
    fn every_text_pair_meets_wcag_aa_in_every_bundled_theme() {
        let mut bad = Vec::new();
        for (name, t) in bundled_with(true) {
            for (label, r, min) in audit(&t) {
                if r + 0.005 < min {
                    bad.push(format!("{name}: {label} = {r:.2} (< {min})"));
                }
            }
        }
        assert!(bad.is_empty(), "{} low-contrast pairs:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn every_text_pair_meets_wcag_aa_with_the_themes_own_chrome() {
        let mut bad = Vec::new();
        for (name, t) in bundled_mode(true, true) {
            for (label, r, min) in audit(&t) {
                if r + 0.005 < min {
                    bad.push(format!("{name}: {label} = {r:.2} (< {min})"));
                }
            }
        }
        assert!(bad.is_empty(), "{} low-contrast pairs:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn own_chrome_adopts_theme_surfaces_that_pass_and_derived_is_the_default() {
        let all_d = bundled_mode(true, false);
        let all_t = bundled_mode(true, true);
        let mut changed = 0;
        let mut kept = 0;
        for ((_, d), (_, t)) in all_d.iter().zip(all_t.iter()) {
            // Derived chrome never moves the bars off the editor background.
            assert_eq!(d.chrome.status_bg, d.background);
            assert_eq!(d.chrome.tabbar_bg, d.background);
            if t.chrome.sidebar_bg != d.chrome.sidebar_bg
                || t.chrome.tab_active_bg != d.chrome.tab_active_bg
                || t.chrome.tabbar_bg != d.chrome.tabbar_bg
                || t.chrome.status_bg != d.chrome.status_bg
            {
                changed += 1;
            } else {
                kept += 1;
            }
        }
        assert!(changed > 20, "only {changed} themes adopted their own chrome");
        assert!(kept > 0, "every theme adopted its chrome: no fallback happens");
    }

    #[test]
    fn the_fix_keeps_already_readable_colors() {
        // github-light-default's foreground and most tokens already pass: they must not move.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes");
        let a = Theme::load_with(&dir.join("github-light-default.json"), false).unwrap();
        let b = Theme::load_with(&dir.join("github-light-default.json"), true).unwrap();
        assert_eq!(a.foreground, b.foreground);
        assert_eq!(a.background, b.background);
    }

    /// `cargo test --release accessibility_report -- --ignored --nocapture`: markdown table with the
    /// worst pair of every theme before and after the fixes (pasted into ROADMAP.md).
    #[test]
    #[ignore]
    fn accessibility_report() {
        println!("| theme | worst pair (before) | before | after |\n|---|---|---:|---:|");
        let (before, after) = (bundled_with(false), bundled_with(true));
        for ((name, tb), (_, ta)) in before.iter().zip(after.iter()) {
            let (ab, aa) = (audit(tb), audit(ta));
            let worst = ab
                .iter()
                .enumerate()
                .min_by(|x, y| (x.1.1 / x.1.2).partial_cmp(&(y.1.1 / y.1.2)).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            let after_worst = aa.iter().map(|x| x.1 / x.2).fold(f32::MAX, f32::min);
            let _ = after_worst;
            println!("| {name} | {} | {:.2} | {:.2} |", ab[worst].0, ab[worst].1, aa.get(worst).map_or(0.0, |x| x.1));
        }
    }
}
