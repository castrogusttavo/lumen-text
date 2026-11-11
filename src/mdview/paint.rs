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
    /// Text selection (accent, translucent).
    pub(super) sel: Rgba,
    /// A link under the pointer.
    pub(super) accent_hot: Rgba,
    /// `<mark>` background (translucent) and the text on it.
    pub(super) mark: Rgba,
    pub(super) on_mark: Rgba,
    /// Tint of a clickable row under the pointer (translucent).
    pub(super) hover: Rgba,
    /// `<kbd>` face, its bottom edge, and the stronger hairline of buttons / key caps.
    pub(super) kbd: Rgba,
    pub(super) kbd_edge: Rgba,
    pub(super) strong: Rgba,
    /// The offending part of a formula that does not parse.
    pub(super) err: Rgba,
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
            sel: c.accent.with_alpha(80),
            accent_hot: c.accent.with_alpha(255).mix(fg, 0.3),
            mark: if bg.luminance() < 0.4 { Rgba(187, 128, 9, 110) } else { Rgba(255, 214, 51, 120) },
            on_mark: fg,
            hover: fg.with_alpha(12),
            kbd: over(0.045),
            kbd_edge: over(0.26),
            strong: over(0.17),
            err: c.error.with_alpha(255).readable_on(&[bg, code], 3.0),
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
            Col::Text => self.text,
            Col::Faint => self.faint,
            Col::Alert(k) => super::alert::color(k, self.bg),
            Col::AlertBg(k) => self.bg.mix(super::alert::color(k, self.bg), 0.075),
        }
    }
}

/// A `<kbd>` key cap around `text`, centered in the run box `r`.
pub(super) fn paint_kbd(cv: &mut Canvas, tx: &mut TextCtx, pal: &Pal, r: Rect, text: &str, clip: Rect) {
    let size = SIZES[SMALL];
    let lh = (size * 1.4).round() as i32;
    let h = (lh + 4).min(r.h - 2).max(4);
    let cap = Rect::new(r.x + 1, r.y + (r.h - h) / 2, (r.w - 2).max(2), h);
    // The heavier bottom edge peeks out under the face, like a real key.
    round_clip(cv, Rect::new(cap.x, cap.y + 1, cap.w, cap.h), 5, pal.kbd_edge, clip);
    card_clip(cv, cap, 5, pal.kbd, pal.strong, clip);
    let tw = tx.ui_width_sized(text, false, size);
    tx.draw_ui_sized(cv, cap.x + (cap.w - tw) / 2, cap.y + (h - lh) / 2, text, pal.fg, clip, false, size);
}

/// A hover button (Copy / Code / Diagram) in `r`.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_button(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    pal: &Pal,
    r: Rect,
    kind: Btn,
    done: bool,
    over: bool,
    clip: Rect,
) {
    let (icon, label) = match kind {
        Btn::Copy(_) if done => ("ui/check", "Copied"),
        Btn::Copy(_) => ("ui/copy", "Copy"),
        Btn::ShowCode => ("ui/code", "Code"),
        Btn::ShowDiagram => ("ui/eye", "Diagram"),
    };
    let fill = if over { pal.bg.mix(pal.fg, 0.11) } else { pal.bg.mix(pal.fg, 0.06) };
    card_clip(cv, r, 6, fill, pal.strong, clip);
    let col = if done { pal.accent } else { pal.text };
    let inner = clip.intersect(&r);
    tx.icon(cv, icon, r.x + 8, r.y + (r.h - 14) / 2, 14, col, inner);
    let lh = (SIZES[TINY] * 1.4).round() as i32;
    tx.draw_ui_sized(cv, r.x + 27, r.y + (r.h - lh) / 2, label, col, inner, false, SIZES[TINY]);
}

/// The `<text>` of an SVG image (not part of its pixels), drawn with the UI font over it.
pub(super) fn paint_svg_texts(cv: &mut Canvas, tx: &mut TextCtx, imgs: &mut ImgStore, id: u32, sr: Rect, clip: Rect) {
    let Some(ts) = imgs.svg_texts(id) else { return };
    let Some((nw, _)) = imgs.size(id) else { return };
    let k = sr.w as f32 / nw.max(1) as f32;
    let clip = clip.intersect(&sr);
    for t in ts.iter() {
        let size = t.size * k;
        if !(4.0..=200.0).contains(&size) || t.color[3] == 0 {
            continue;
        }
        let w = tx.ui_width_sized(&t.text, t.bold, size) as f32;
        let x = sr.x as f32 + t.x * k
            - match t.anchor {
                0 => w / 2.0,
                1 => w,
                _ => 0.0,
            };
        // SVG places the baseline; the UI text is drawn from the top of its line box.
        let y = sr.y as f32 + t.y * k - size * 1.05;
        let c = Rgba(t.color[0], t.color[1], t.color[2], t.color[3]);
        tx.draw_ui_sized(cv, x.round() as i32, y.round() as i32, &t.text, c, clip, t.bold, size);
    }
}

/// Theme color of a diagram paint.
pub(super) fn dia_color(pal: &Pal, p: mermaid::Paint) -> Rgba {
    use mermaid::Paint as P;
    let accent = pal.accent;
    match p {
        P::Node => pal.bg.mix(accent, 0.13),
        P::NodeBorder => pal.bg.mix(accent, 0.62),
        P::Line => pal.bg.mix(pal.fg, 0.55),
        P::Text => pal.fg,
        P::Muted => pal.dim,
        P::Cluster => pal.bg.mix(pal.fg, 0.035),
        P::ClusterBorder => pal.bg.mix(pal.fg, 0.2),
        P::Note => pal.bg.mix(Rgba(0xe3, 0xb3, 0x41, 255), 0.18),
        P::NoteBorder => pal.bg.mix(Rgba(0xe3, 0xb3, 0x41, 255), 0.6),
        P::LabelBg => pal.bg,
        P::Series(i) => series(pal.bg, i),
        P::OnSeries(i) => {
            let c = series(pal.bg, i);
            let (w, b) = (Rgba(255, 255, 255, 255), Rgba(20, 20, 24, 255));
            if w.contrast(c) >= b.contrast(c) { w } else { b }
        }
    }
}

/// Pie / series palette: distinct hues that read on both page colors.
fn series(bg: Rgba, i: u8) -> Rgba {
    const DARK: [Rgba; 8] = [
        Rgba(0x4c, 0x8e, 0xf7, 255),
        Rgba(0x3f, 0xb9, 0x50, 255),
        Rgba(0xe3, 0xa0, 0x08, 255),
        Rgba(0xab, 0x7d, 0xf8, 255),
        Rgba(0xf7, 0x78, 0x5e, 255),
        Rgba(0x2b, 0xb3, 0xb3, 255),
        Rgba(0xe8, 0x6f, 0xb4, 255),
        Rgba(0x9a, 0xa5, 0xb1, 255),
    ];
    const LIGHT: [Rgba; 8] = [
        Rgba(0x21, 0x6e, 0xe0, 255),
        Rgba(0x1f, 0x88, 0x3d, 255),
        Rgba(0xbf, 0x87, 0x00, 255),
        Rgba(0x82, 0x50, 0xdf, 255),
        Rgba(0xcf, 0x4a, 0x2f, 255),
        Rgba(0x10, 0x8a, 0x8a, 255),
        Rgba(0xbf, 0x3d, 0x8c, 255),
        Rgba(0x6e, 0x77, 0x81, 255),
    ];
    let pal = if bg.luminance() < 0.4 { DARK } else { LIGHT };
    pal[i as usize % pal.len()]
}

/// A Mermaid diagram: its shapes rasterized once per size and theme, its labels drawn as UI text.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_diagram(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    pal: &Pal,
    cache: &mut HashMap<(u64, i32, i32, u64), Rc<Pix>>,
    theme_key: u64,
    key: u64,
    dia: &mermaid::Diagram,
    sr: Rect,
    clip: Rect,
) {
    let dev = cv.to_dev(sr);
    if dev.w <= 0 || dev.h <= 0 {
        return;
    }
    let ck = (key, dev.w, dev.h, theme_key);
    let pix = match cache.get(&ck) {
        Some(p) => p.clone(),
        None => {
            let Some(p) = mermaid::render::raster(dia, dev.w as u32, dev.h as u32, &|c| dia_color(pal, c)) else {
                return;
            };
            let p = Rc::new(p);
            cache.insert(ck, p.clone());
            p
        }
    };
    let dclip = cv.to_dev(clip);
    crate::mdimg::blit(cv, dev.x, dev.y, &pix, dclip);
    let k = sr.w as f32 / dia.w.max(1.0);
    for l in &dia.labels {
        let size = l.size * k;
        if size < 4.0 || l.text.is_empty() {
            continue;
        }
        let w = tx.ui_width_sized(&l.text, l.bold, size) as f32;
        let x = sr.x as f32 + l.x * k
            - match l.anchor {
                0 => w / 2.0,
                1 => w,
                _ => 0.0,
            };
        let y = sr.y as f32 + l.y * k - (size * 1.4).round() / 2.0;
        tx.draw_ui_sized(cv, x.round() as i32, y.round() as i32, &l.text, dia_color(pal, l.col), clip, l.bold, size);
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
