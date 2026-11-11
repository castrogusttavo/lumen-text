//! Font settings shared by the whole editor: two independent groups, the interface (UI) font and
//! the buffer font (code editor and everything monospace: terminal, output, diffs, code blocks).
//!
//! This module holds the pure parts: value cleaning, line-height math, the lazily built list of
//! installed families (from fontconfig) and the globals that layout code reads. `TextCtx` (in
//! `ui.rs`) turns a choice into loaded faces.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

pub const DEFAULT_BUFFER_SIZE: f32 = 14.0;
pub const DEFAULT_UI_SIZE: f32 = 13.0;
/// 22/14: the ratio the editor always used (a 14 px font on a 22 px row).
pub const DEFAULT_BUFFER_LH: f32 = 1.57;
/// 18/13: the interface's historical text box.
pub const DEFAULT_UI_LH: f32 = 1.38;
pub const BUFFER_SIZE_RANGE: (f32, f32) = (8.0, 40.0);
pub const UI_SIZE_RANGE: (f32, f32) = (10.0, 20.0);
/// Line heights up to this value are multipliers of the font size; larger ones are pixels.
pub const LH_MULT_MAX: f32 = 4.0;
pub const LH_RANGE: (f32, f32) = (1.0, 2.5);
pub const LH_PX_RANGE: (f32, f32) = (8.0, 80.0);

/// One font group as stored in the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// "" = the system default (monospace for the buffer, sans-serif for the UI).
    pub family: String,
    pub size: f32,
    /// 100..=900 in steps of 100.
    pub weight: u16,
    /// Multiplier of the size (1.0..=2.5) or pixels (> 4).
    pub line_height: f32,
    pub ligatures: bool,
    /// OpenType features on top of the ligature switch (`zero` -> 1, `cv02` -> 0, ...); buffer only.
    pub features: BTreeMap<String, u32>,
    /// Extra advance per character in logical px (see [`LETTER_SPACING_RANGE`]); buffer only.
    pub letter_spacing: f32,
}

impl FontSpec {
    pub fn buffer_default() -> Self {
        Self {
            family: String::new(),
            size: DEFAULT_BUFFER_SIZE,
            weight: 400,
            line_height: DEFAULT_BUFFER_LH,
            ligatures: false,
            features: BTreeMap::new(),
            letter_spacing: 0.0,
        }
    }
    pub fn ui_default() -> Self {
        Self {
            family: String::new(),
            size: DEFAULT_UI_SIZE,
            weight: 400,
            line_height: DEFAULT_UI_LH,
            ligatures: true,
            features: BTreeMap::new(),
            letter_spacing: 0.0,
        }
    }
}

/// A weight setting rounded to the nearest hundred inside 100..=900.
pub fn clean_weight(w: i64) -> u16 {
    (((w.clamp(100, 900) + 50) / 100) * 100) as u16
}

/// A size inside `range` (NaN falls back to `default`).
pub fn clean_size(v: f64, range: (f32, f32), default: f32) -> f32 {
    if v.is_finite() { (v as f32).clamp(range.0, range.1) } else { default }
}

/// A line-height setting made valid: multipliers are clamped to 1.0..=2.5, pixel values (> 4) to
/// 8..=80 px; anything non-finite becomes `default`.
pub fn clean_line_height(v: f64, default: f32) -> f32 {
    if !v.is_finite() || v <= 0.0 {
        return default;
    }
    let v = v as f32;
    if v > LH_MULT_MAX { v.clamp(LH_PX_RANGE.0, LH_PX_RANGE.1).round() } else { v.clamp(LH_RANGE.0, LH_RANGE.1) }
}

/// Row height in logical pixels for a font `size` and a line-height setting.
pub fn line_px(size: f32, lh: f32) -> f32 {
    let px = if lh > LH_MULT_MAX { lh } else { size * lh };
    px.round().max(1.0)
}

/// Bounds of the buffer letter spacing in logical px.
pub const LETTER_SPACING_RANGE: (f32, f32) = (-1.0, 3.0);

/// A letter-spacing setting inside [`LETTER_SPACING_RANGE`], rounded to 0.05 px (NaN -> 0).
pub fn clean_letter_spacing(v: f64) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    ((v as f32).clamp(LETTER_SPACING_RANGE.0, LETTER_SPACING_RANGE.1) * 20.0).round() / 20.0
}

/// An OpenType feature tag as stored: exactly 4 ASCII letters or digits, lowercased.
pub fn clean_feature_tag(t: &str) -> Option<String> {
    let t = t.trim();
    (t.len() == 4 && t.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| t.to_ascii_lowercase())
}

/// `buffer_font_features` from config.json: `{"zero": true, "cv02": false, "cv05": 3}`. Booleans
/// mean on / off, numbers pick an alternate (0..=99); anything else is dropped.
pub fn clean_features(v: &serde_json::Value) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    for (k, x) in v.as_object().into_iter().flatten() {
        let val = match x {
            serde_json::Value::Bool(b) => Some(*b as u32),
            n => n.as_u64().map(|n| n.min(99) as u32),
        };
        if let (Some(tag), Some(val)) = (clean_feature_tag(k), val) {
            m.insert(tag, val);
        }
    }
    m
}

/// The features as config.json stores them: true / false for 1 / 0, a number otherwise.
pub fn features_json(m: &BTreeMap<String, u32>) -> serde_json::Value {
    let o: serde_json::Map<String, serde_json::Value> = m
        .iter()
        .map(|(k, v)| {
            let x = match v {
                0 => serde_json::Value::Bool(false),
                1 => serde_json::Value::Bool(true),
                n => serde_json::json!(n),
            };
            (k.clone(), x)
        })
        .collect();
    serde_json::Value::Object(o)
}

/// The one-line text form the settings field edits: `zero ss01 -cv02 cv05=3` (`-` switches a
/// feature off, `=n` picks an alternate; commas work as separators too). Bad words are skipped.
pub fn parse_features_text(s: &str) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    for w in s.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|w| !w.is_empty()) {
        let (w, off) = match w.strip_prefix('-') {
            Some(r) => (r, true),
            None => (w.strip_prefix('+').unwrap_or(w), false),
        };
        let (tag, val) = match w.split_once('=') {
            Some((t, v)) => match v.trim() {
                "true" | "on" => (t, Some(1)),
                "false" | "off" => (t, Some(0)),
                n => (t, n.parse::<u32>().ok().map(|n| n.min(99))),
            },
            None => (w, Some(if off { 0 } else { 1 })),
        };
        if let (Some(tag), Some(val)) = (clean_feature_tag(tag), val) {
            m.insert(tag, val);
        }
    }
    m
}

/// Inverse of [`parse_features_text`].
pub fn features_text(m: &BTreeMap<String, u32>) -> String {
    m.iter()
        .map(|(k, v)| match v {
            0 => format!("-{k}"),
            1 => k.clone(),
            n => format!("{k}={n}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Family names with whitespace trimmed and control characters removed.
pub fn clean_family(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(120).collect()
}

/// The name shown for a weight.
pub fn weight_name(w: u16) -> &'static str {
    match w {
        ..=100 => "Thin",
        101..=200 => "Extra Light",
        201..=300 => "Light",
        301..=400 => "Regular",
        401..=500 => "Medium",
        501..=600 => "Semi Bold",
        601..=700 => "Bold",
        701..=800 => "Extra Bold",
        _ => "Black",
    }
}

/// Weight used for bold runs on top of a base weight: always visibly heavier, 900 at most.
pub fn bold_weight(base: u16, add: u16, floor: u16) -> u16 {
    (base + add).clamp(floor, 900)
}

// ── globals read by layout code ──
//
// Slots: 0 buffer line height, 1 UI size, 2 UI line height, 3 buffer size. In tests every thread
// has its own copy (parallel tests that change fonts never see each other).

const SLOT_BUF_LH: usize = 0;
const SLOT_UI_SIZE: usize = 1;
const SLOT_UI_LH: usize = 2;
const SLOT_BUF_SIZE: usize = 3;
const DEFAULTS: [f32; 4] = [DEFAULT_BUFFER_LH, DEFAULT_UI_SIZE, DEFAULT_UI_LH, DEFAULT_BUFFER_SIZE];

#[cfg(not(test))]
mod store {
    use super::DEFAULTS;
    use std::sync::atomic::{AtomicU32, Ordering};
    static SLOTS: [AtomicU32; 4] = [
        AtomicU32::new(DEFAULTS[0].to_bits()),
        AtomicU32::new(DEFAULTS[1].to_bits()),
        AtomicU32::new(DEFAULTS[2].to_bits()),
        AtomicU32::new(DEFAULTS[3].to_bits()),
    ];
    static GEN: AtomicU32 = AtomicU32::new(0);
    pub fn get(i: usize) -> f32 {
        f32::from_bits(SLOTS[i].load(Ordering::Relaxed))
    }
    pub fn set(i: usize, v: f32) {
        SLOTS[i].store(v.to_bits(), Ordering::Relaxed)
    }
    pub fn generation() -> u32 {
        GEN.load(Ordering::Relaxed)
    }
    pub fn bump() {
        GEN.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod store {
    use super::DEFAULTS;
    use std::cell::Cell;
    thread_local! {
        static SLOTS: [Cell<f32>; 4] =
            [Cell::new(DEFAULTS[0]), Cell::new(DEFAULTS[1]), Cell::new(DEFAULTS[2]), Cell::new(DEFAULTS[3])];
        static GEN: Cell<u32> = const { Cell::new(0) };
    }
    pub fn get(i: usize) -> f32 {
        SLOTS.with(|s| s[i].get())
    }
    pub fn set(i: usize, v: f32) {
        SLOTS.with(|s| s[i].set(v))
    }
    pub fn generation() -> u32 {
        GEN.with(Cell::get)
    }
    pub fn bump() {
        GEN.with(|g| g.set(g.get() + 1))
    }
}

pub fn buffer_line_height() -> f32 {
    store::get(SLOT_BUF_LH)
}
pub fn buffer_size() -> f32 {
    store::get(SLOT_BUF_SIZE)
}
pub fn ui_size() -> f32 {
    store::get(SLOT_UI_SIZE)
}
pub fn ui_line_height() -> f32 {
    store::get(SLOT_UI_LH)
}
pub(crate) fn store_buffer_line_height(v: f32) {
    store::set(SLOT_BUF_LH, v);
    bump();
}
pub(crate) fn store_buffer_size(v: f32) {
    store::set(SLOT_BUF_SIZE, v);
    bump();
}
pub(crate) fn store_ui_size(v: f32) {
    store::set(SLOT_UI_SIZE, v);
    bump();
}
pub(crate) fn store_ui_line_height(v: f32) {
    store::set(SLOT_UI_LH, v);
    bump();
}

/// Counter that changes whenever any font setting changes: caches keyed on text metrics include it.
pub fn generation() -> u32 {
    store::generation()
}
pub(crate) fn bump() {
    store::bump()
}

/// UI text box height (the chrome's single-line text always uses the historical 18/13 ratio, so
/// rows keep their alignment whatever the UI line height).
pub fn ui_line() -> f32 {
    (ui_size() * 18.0 / 13.0).round()
}

/// Multiplier for multi-line interface prose (hover cards): the UI line height relative to its default.
pub fn prose_scale() -> f32 {
    ui_line_height() / DEFAULT_UI_LH
}

// ── installed fonts ──

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub name: String,
    pub mono: bool,
    /// The face best suited to preview the family: upright, closest to regular weight.
    pub file: PathBuf,
}

/// Monospace guess from a family name, for fonts that do not advertise `spacing=mono`.
fn mono_by_name(name: &str) -> bool {
    let n = name.to_lowercase();
    ["mono", "code", "courier", "consolas", "terminal", "typewriter", "hack", "menlo", "fixed", "console"]
        .iter()
        .any(|k| n.contains(k))
        && !n.contains("propo")
}

/// Parse `fc-list -f '%{family[0]}\t%{spacing}\t%{slant}\t%{weight}\t%{file}\n'` output into a sorted,
/// de-duplicated list.
pub fn parse_families(out: &str) -> Vec<Family> {
    // name (lowercase) -> (family, best score so far)
    let mut map: std::collections::BTreeMap<String, (Family, (bool, i32))> = Default::default();
    for line in out.lines() {
        let mut it = line.split('\t');
        let name = it.next().unwrap_or("").trim();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        let spacing = it.next().unwrap_or("").trim();
        let slant: i32 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
        let weight: i32 = it.next().unwrap_or("80").trim().parse().unwrap_or(80);
        let file = PathBuf::from(it.next().unwrap_or("").trim());
        let mono = matches!(spacing, "100" | "110") || mono_by_name(name);
        let score = (slant != 0, (weight - 80).abs());
        let e = map
            .entry(name.to_lowercase())
            .or_insert_with(|| (Family { name: name.to_string(), mono: false, file: file.clone() }, (true, i32::MAX)));
        e.0.mono |= mono;
        if score < e.1 {
            e.1 = score;
            e.0.file = file;
        }
    }
    map.into_values().map(|(f, _)| f).collect()
}

/// Every installed family (first name of each font), computed once on first use (one `fc-list`
/// call, a few ms) and kept for the session.
pub fn installed_families() -> &'static [Family] {
    static CACHE: OnceLock<Vec<Family>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Command::new("fc-list")
            .args(["-f", "%{family[0]}\t%{spacing}\t%{slant}\t%{weight}\t%{file}\n"])
            .output()
            .map(|o| parse_families(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    })
}

/// Escape a family name for a fontconfig pattern.
fn fc_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '\\' | '-' | ':' | ',' | '=' | '(' | ')' | '[' | ']') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Font files of `family` (all weights and slants), at most `max` and none above `max_bytes`.
pub fn family_files(family: &str, max: usize, max_bytes: u64) -> Vec<PathBuf> {
    let Ok(out) = Command::new("fc-list").args(["-f", "%{file}\n", &fc_escape(family)]).output() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= max_bytes))
        .collect();
    files.sort();
    files.dedup();
    files.truncate(max);
    files
}

/// Case-insensitive, substring match of every word of `query` in `name` (the picker's filter).
pub fn matches_query(name: &str, query: &str) -> bool {
    let n = name.to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| n.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_snap_to_hundreds_in_range() {
        assert_eq!(clean_weight(0), 100);
        assert_eq!(clean_weight(449), 400);
        assert_eq!(clean_weight(450), 500);
        assert_eq!(clean_weight(2000), 900);
        assert_eq!(weight_name(400), "Regular");
        assert_eq!(weight_name(700), "Bold");
        assert_eq!(weight_name(100), "Thin");
        assert_eq!(weight_name(900), "Black");
    }

    #[test]
    fn line_height_multiplier_or_pixels() {
        assert_eq!(clean_line_height(0.2, 1.57), 1.0);
        assert_eq!(clean_line_height(3.0, 1.57), 2.5);
        assert_eq!(clean_line_height(1.8, 1.57), 1.8);
        assert_eq!(clean_line_height(22.0, 1.57), 22.0, "above 4 means pixels");
        assert_eq!(clean_line_height(500.0, 1.57), 80.0);
        assert_eq!(clean_line_height(f64::NAN, 1.57), 1.57);
        assert_eq!(clean_line_height(-1.0, 1.57), 1.57);
        assert_eq!(line_px(14.0, 22.0), 22.0);
        assert_eq!(line_px(14.0, 2.0), 28.0);
    }

    #[test]
    fn the_default_row_height_is_unchanged_for_every_integer_size() {
        for s in 8..=40 {
            let old = (s as f32 * 22.0 / 14.0).round();
            assert_eq!(line_px(s as f32, DEFAULT_BUFFER_LH), old, "size {s}");
        }
        assert_eq!(line_px(13.0, DEFAULT_UI_LH), 18.0);
    }

    #[test]
    fn bold_is_heavier_but_capped() {
        assert_eq!(bold_weight(400, 300, 700), 700);
        assert_eq!(bold_weight(400, 200, 600), 600, "the UI's semibold");
        assert_eq!(bold_weight(700, 300, 700), 900);
        assert_eq!(bold_weight(900, 300, 700), 900);
        assert_eq!(bold_weight(100, 200, 600), 600);
    }

    #[test]
    fn family_list_is_sorted_deduped_and_marks_monospace() {
        let out = "Noto Sans\t0\t0\t80\t/n.ttf\nJetBrains Mono\t100\t100\t200\t/jb-bold-italic.ttf\nJetBrains Mono\t100\t0\t80\t/jb.ttf\njetbrains mono\t\t0\t100\t/x.ttf\n.Hidden\t\t0\t80\t/h.ttf\nFira Code\t\t0\t80\t/f.ttf\nGeistMono NF Propo\t\t0\t80\t/g.ttf\n\t\nAdwaita Mono\t100\t0\t80\t/a.ttf\n";
        let f = parse_families(out);
        let names: Vec<&str> = f.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Adwaita Mono", "Fira Code", "GeistMono NF Propo", "JetBrains Mono", "Noto Sans"]);
        let mono: Vec<bool> = f.iter().map(|x| x.mono).collect();
        assert_eq!(mono, [true, true, false, true, false]);
        let jb = f.iter().find(|x| x.name == "JetBrains Mono").unwrap();
        assert_eq!(jb.file, PathBuf::from("/jb.ttf"), "the upright regular face previews the family");
    }

    #[test]
    fn picker_filter_is_word_based() {
        assert!(matches_query("JetBrains Mono", "jet mono"));
        assert!(!matches_query("JetBrains Mono", "fira"));
        assert!(matches_query("Fira Code", ""));
    }

    #[test]
    fn font_features_parse_clean_and_round_trip() {
        let m = parse_features_text("zero, +SS01 -cv02 cv05=3 calt=off bad toolong -x ss02=nope");
        let got: Vec<(&str, u32)> = m.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(got, [("calt", 0), ("cv02", 0), ("cv05", 3), ("ss01", 1), ("zero", 1)]);
        assert_eq!(features_text(&m), "-calt -cv02 cv05=3 ss01 zero");
        assert_eq!(parse_features_text(&features_text(&m)), m);
        assert!(parse_features_text("").is_empty());
        let j = features_json(&m);
        assert_eq!(j["zero"], serde_json::json!(true));
        assert_eq!(j["cv02"], serde_json::json!(false));
        assert_eq!(j["cv05"], serde_json::json!(3));
        assert_eq!(clean_features(&j), m);
        assert!(clean_features(&serde_json::json!("zero")).is_empty());
        assert_eq!(clean_feature_tag(" Zero "), Some("zero".into()));
        assert_eq!(clean_feature_tag("ss-1"), None);
    }

    #[test]
    fn letter_spacing_is_clamped_and_rounded() {
        assert_eq!(clean_letter_spacing(0.0), 0.0);
        assert_eq!(clean_letter_spacing(-5.0), -1.0);
        assert_eq!(clean_letter_spacing(9.0), 3.0);
        assert_eq!(clean_letter_spacing(0.52), 0.5);
        assert_eq!(clean_letter_spacing(f64::NAN), 0.0);
    }

    #[test]
    fn family_escape_protects_pattern_syntax() {
        assert_eq!(fc_escape("A-B: C"), "A\\-B\\: C");
        assert_eq!(clean_family("  Fira\u{0} Code  "), "Fira Code");
    }
}
