use super::*;

// ---------------------------------------------------------------------------------------------
// clipped drawing helpers (Canvas::fill_round & co. only clip to the canvas, not to the pane)

pub(super) fn round_clip(cv: &mut Canvas, r: Rect, rad: i32, c: Rgba, clip: Rect) {
    cv.fill_round_clip(r, rad, c, clip);
}

pub(super) fn card_clip(cv: &mut Canvas, r: Rect, rad: i32, fill: Rgba, border: Rgba, clip: Rect) {
    round_clip(cv, r, rad, border, clip);
    round_clip(cv, r.inset(1), (rad - 1).max(0), fill, clip);
}

pub(super) fn stroke_clip(cv: &mut Canvas, r: Rect, c: Rgba, clip: Rect) {
    cv.fill_clip(Rect::new(r.x, r.y, r.w, 1), c, clip);
    cv.fill_clip(Rect::new(r.x, r.bottom() - 1, r.w, 1), c, clip);
    cv.fill_clip(Rect::new(r.x, r.y, 1, r.h), c, clip);
    cv.fill_clip(Rect::new(r.right() - 1, r.y, 1, r.h), c, clip);
}

/// Thick polyline check mark centered in the box `r` (drawn for 14 px and scaled to the box).
pub(super) fn check_mark(cv: &mut Canvas, r: Rect, c: Rgba, clip: Rect) {
    let k = r.w as f32 / 14.0;
    let pts = [(3.5f32, 7.3f32), (6.0, 9.8), (10.6, 4.4)];
    for seg in pts.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        for i in 0..=24 {
            let t = i as f32 / 24.0;
            let (px, py) = (r.x + ((a.0 + (b.0 - a.0) * t) * k) as i32, r.y + ((a.1 + (b.1 - a.1) * t) * k) as i32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                if clip.contains((px + dx) as f64, (py + dy) as f64) {
                    cv.dot(px + dx, py + dy, c);
                }
            }
        }
    }
}

/// Preview palette. Neutral surfaces are the foreground laid over the page at a low alpha (like
/// `color-mix(fg N%, transparent)`), so code, rules and tables keep the same contrast on every theme
/// instead of depending on how the theme picked its widget colors.
pub(super) struct Pal {
    /// Headings, bold text, table headers.
    pub(super) fg: Rgba,
    /// Body prose: the foreground at ~80%.
    pub(super) text: Rgba,
    /// Muted (~54%): list numbers, footnote marks, H6, checkbox outlines.
    pub(super) dim: Rgba,
    /// Barely there: the language label of a code block.
    pub(super) faint: Rgba,
    pub(super) accent: Rgba,
    pub(super) accent_soft: Rgba,
    pub(super) on_accent: Rgba,
    /// Hairlines: table grid, footnote rule, image card border (fg ~9%).
    pub(super) border: Rgba,
    /// Thematic break (fg ~18%).
    pub(super) rule: Rgba,
    /// Block quote bar (fg ~19%).
    pub(super) bar: Rgba,
    /// Inline code chips and code blocks (fg ~5.5%).
    pub(super) code: Rgba,
    /// Table header row (fg ~6%).
    pub(super) head: Rgba,
    pub(super) widget: Rgba,
    pub(super) input: Rgba,
    pub(super) bg: Rgba,
}

impl Pal {
    pub(super) fn new(t: &Theme) -> Self {
        let bg = t.background.with_alpha(255);
        let fg = t.foreground.with_alpha(255);
        let c = &t.chrome;
        let over = |a: f32| bg.mix(fg, a);
        // Softened text keeps its contrast: body text at least 7:1 on the page (or the theme's own fg
        // contrast when that is lower), muted text at least AA; both stay AA on a code chip.
        let code = over(0.055);
        let reads = |col: Rgba, min: f32| col.contrast(bg) >= min && col.contrast(code) >= 4.5;
        let text = if reads(over(0.8), fg.contrast(bg).min(7.0)) { over(0.8) } else { fg };
        let dim = if reads(over(0.54), 4.5) { over(0.54) } else { over(0.54).readable_on(&[bg, code], 4.5) };
        Pal {
            fg,
            text,
            dim,
            faint: over(0.38),
            accent: c.accent.with_alpha(255),
            accent_soft: c.accent.with_alpha(70),
            on_accent: c.on_accent.with_alpha(255),
            border: over(0.09),
            rule: over(0.18),
            bar: over(0.19),
            code,
            head: over(0.06),
            widget: c.widget_bg.with_alpha(255),
            input: c.input_bg.with_alpha(255),
            bg,
        }
    }
    /// Chip behind inline math: the page tinted with the accent, as long as `text` reads on it.
    pub(super) fn math_chip(&self, text: Rgba) -> Rgba {
        let c = self.bg.mix(self.accent, 0.13);
        if text.contrast(c) >= 4.5 { c } else { self.code }
    }
    /// Fill of a `$$` block.
    pub(super) fn math_block(&self) -> Rgba {
        let c = self.code.mix(self.accent, 0.07);
        if self.fg.contrast(c) >= 4.5 { c } else { self.code }
    }
    pub(super) fn get(&self, c: Col) -> Rgba {
        match c {
            Col::Fg => self.fg,
            Col::Dim => self.dim,
            Col::Accent => self.accent,
            Col::AccentSoft => self.accent_soft,
            Col::Border => self.border,
            Col::Rule => self.rule,
            Col::Bar => self.bar,
            Col::Code => self.code,
            Col::Head => self.head,
            Col::Widget => self.widget,
            Col::Input => self.input,
            Col::Bg => self.bg,
        }
    }
}

/// Colored runs of a code block, asked from the host once and cached. Colors that would not read
/// on the block's own background are nudged (the theme's tokens are checked against the editor's).
pub(super) fn hl_lines(
    cache: &mut HashMap<u64, Option<Rc<HlLines>>>,
    key: u64,
    lang: &str,
    lines: &[String],
    host: &mut Option<&mut HlFn>,
    fill: Rgba,
    bg: Rgba,
) -> Option<Rc<HlLines>> {
    if let Some(v) = cache.get(&key) {
        return v.clone();
    }
    let h = host.as_mut()?;
    let mut res = h(lang, &lines.join("\n"));
    if let Some(runs) = res.as_mut() {
        for line in runs.iter_mut() {
            for r in line.iter_mut() {
                if r.2.contrast(fill) < 4.5 {
                    r.2 = r.2.readable_on(&[fill, bg], 4.5);
                }
            }
        }
    }
    let res = res.map(Rc::new);
    cache.insert(key, res.clone());
    res
}

/// Pieces to draw for `line` (already cut to what is visible) from highlighter `runs`.
pub(super) fn pieces_of(line: &str, runs: &[(usize, usize, Rgba, u8)], fg: Rgba) -> Vec<(String, Rgba, u8)> {
    let mut out: Vec<(String, Rgba, u8)> = vec![];
    let mut at = 0;
    for &(s, e, c, f) in runs {
        let (s, e) = (s.min(line.len()), e.min(line.len()));
        if s >= e || s < at || !line.is_char_boundary(s) || !line.is_char_boundary(e) {
            continue;
        }
        if s > at {
            out.push((line[at..s].to_string(), fg, 0));
        }
        out.push((line[s..e].to_string(), c, f));
        at = e;
    }
    if at < line.len() {
        out.push((line[at..].to_string(), fg, 0));
    }
    out
}
