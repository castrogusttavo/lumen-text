//! Shared look and motion of the editor's floating popups (hover, completion, docs card): one
//! card style (`R_CARD`, soft shadow, faint accent ring), one entrance (a `dur::BASE` fade) and a
//! few pure helpers for the content (matched-character runs, word wrapping, kind icons).
//!
//! The canvas has no group opacity, so the fade is done on the pixels: the popup area is saved
//! before the popup is drawn (`Canvas::snap_region`) and, once it is drawn, blended back toward
//! the saved pixels (`Canvas::blend_over_snap`, shared with the modal entrance). The caret line
//! never moves: nothing is laid out again.

use crate::anim::{Tween, dur};
use crate::ds;
use crate::theme::Chrome;
use crate::ui::{Canvas, Rect};

/// Entrance of a popup: 0 -> 1 over `dur::BASE`, advanced by the frame time.
#[derive(Clone, Copy, Debug)]
pub struct PopupFade(Tween);

impl Default for PopupFade {
    /// A popup is born fading in.
    fn default() -> Self {
        let mut t = Tween::new(dur::BASE);
        t.start();
        Self(t)
    }
}

impl PopupFade {
    /// Advance by `dt`; true while still fading (the caller folds it into `busy`).
    pub fn step(&mut self, dt: f64) -> bool {
        self.0.step(dt)
    }

    /// Eased opacity of the popup, 0..1.
    pub fn k(&self) -> f32 {
        self.0.eased()
    }
}

impl Canvas<'_> {
    /// A floating editor popup: the shared floating card at popover elevation.
    pub fn pop_card(&mut self, r: Rect, c: &Chrome) {
        self.float_card(r, ds::R_CARD, c, ds::Elev::Pop);
    }
}

/// Area a popup's shadow can reach around its rect (for [`Canvas::grab`]).
pub fn shadow_area(r: Rect) -> Rect {
    Rect::new(r.x - 20, r.y - 20, r.w + 40, r.h + 52)
}

/// Split `text` into runs of (chars, matched?) given the matched char positions (ascending).
pub fn match_runs(text: &str, matched: &[usize]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let m = matched.binary_search(&i).is_ok();
        match out.last_mut() {
            Some((s, on)) if *on == m => s.push(ch),
            _ => out.push((ch.to_string(), m)),
        }
    }
    out
}

/// Greedy word wrap to `max_w` (measured by `width`), at most `max_lines` lines; when text is
/// left over the last line ends with "…". Blank source lines stay as paragraph gaps.
pub fn wrap_words(text: &str, max_w: i32, max_lines: usize, mut width: impl FnMut(&str) -> i32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cut = false;
    let mut paras = text.lines();
    'src: while let Some(para) = paras.next() {
        if para.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
        } else {
            let mut cur = String::new();
            for word in para.split_whitespace() {
                let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if cur.is_empty() || width(&trial) <= max_w {
                    cur = trial;
                    continue;
                }
                lines.push(std::mem::take(&mut cur));
                if lines.len() >= max_lines {
                    cut = true;
                    break 'src;
                }
                cur = word.to_string();
            }
            lines.push(cur);
        }
        if lines.len() >= max_lines {
            cut = paras.any(|p| !p.trim().is_empty());
            break;
        }
    }
    lines.truncate(max_lines);
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if cut && let Some(l) = lines.last_mut() {
        l.push('…');
    }
    lines
}

/// Bundled icon for an LSP `CompletionItemKind`.
pub fn kind_icon(kind: u32) -> &'static str {
    match kind {
        2..=4 => "ui/bolt_outlined",
        5 | 10 => "ui/square_dot",
        6 => "ui/at_sign",
        7 | 22 => "ui/box",
        8 => "ui/blocks",
        9 => "ui/folder",
        13 | 20 => "ui/list_tree",
        14 => "ui/code",
        15 => "ui/text_snippet",
        21 => "ui/hash",
        25 => "ui/binary",
        _ => "ui/circle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_runs_from_zero_to_one() {
        let mut f = PopupFade::default();
        assert_eq!(f.k(), 0.0);
        let mut n = 0;
        while f.step(0.05) && n < 100 {
            n += 1;
        }
        assert_eq!(f.k(), 1.0);
    }

    #[test]
    fn runs_group_matched_chars() {
        assert_eq!(
            match_runs("getName", &[0, 3, 4]),
            vec![
                ("g".to_string(), true),
                ("et".to_string(), false),
                ("Na".to_string(), true),
                ("me".to_string(), false)
            ]
        );
        assert_eq!(match_runs("ab", &[]), vec![("ab".to_string(), false)]);
        assert!(match_runs("", &[1]).is_empty());
    }

    #[test]
    fn wrap_limits_lines_and_marks_the_cut() {
        let w = |s: &str| s.chars().count() as i32;
        assert_eq!(wrap_words("aa bb cc dd", 5, 10, w), vec!["aa bb", "cc dd"]);
        assert_eq!(wrap_words("aa bb cc dd", 5, 1, w), vec!["aa bb…"]);
        assert_eq!(wrap_words("one", 5, 3, w), vec!["one"]);
        assert!(wrap_words("   ", 5, 3, w).is_empty());
    }

    #[test]
    fn kind_icons_exist_in_the_bundle() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/ui");
        let stems: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().to_string()))
            .collect();
        for k in [2, 5, 6, 7, 8, 9, 13, 14, 15, 21, 25, 0] {
            let stem = kind_icon(k).trim_start_matches("ui/");
            assert!(stems.iter().any(|s| s == stem), "{stem} missing");
        }
    }
}
