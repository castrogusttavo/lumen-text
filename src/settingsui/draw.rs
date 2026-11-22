use super::*;
use crate::anim::{fade, lerp, lerp_rgba};

// ───────────────────────────── drawing helpers ─────────────────────────────

/// Rounded fill that degrades to a clipped square fill when the shape is cut by `clip`.
pub(super) fn rr(cv: &mut Canvas, r: Rect, rad: i32, col: Rgba, clip: Rect) {
    if r.intersect(&clip) == r {
        cv.fill_round(r, rad, col);
    } else {
        cv.fill_clip(r, col, clip);
    }
}

pub(super) fn card_c(cv: &mut Canvas, r: Rect, rad: i32, fill: Rgba, border: Rgba, clip: Rect) {
    if r.intersect(&clip) == r {
        cv.card(r, rad, fill, border);
    } else {
        cv.fill_clip(r, border, clip);
        cv.fill_clip(r.inset(1), fill, clip);
    }
}

pub(super) fn on_accent(a: Rgba) -> Rgba {
    if a.luminance() > 0.62 { Rgba(20, 20, 20, 255) } else { Rgba(255, 255, 255, 255) }
}

/// `s` shortened with an ellipsis so it fits in `max_w` pixels.
pub(super) fn fit(tx: &mut TextCtx, s: &str, max_w: i32, bold: bool) -> String {
    crate::ui::fit_ellipsis(tx, s, max_w, bold)
}

pub(super) fn push_hit(hits: &mut Vec<(Rect, Hit)>, r: Rect, clip: Rect, h: Hit) {
    let r = r.intersect(&clip);
    if r.w > 0 && r.h > 0 {
        hits.push((r, h));
    }
}

/// The dropdown's thin bar. Follows the `scrollbar` setting: hidden with "never"; with "auto" the
/// open dropdown counts as being pointed at (it only exists while the user works in it).
pub(super) fn scrollbar(cv: &mut Canvas, area: Rect, scroll: f64, content: i32, col: Rgba) {
    if content <= area.h || area.h <= 0 || crate::display::bar_strength(true) <= 0.0 {
        return;
    }
    let th = ((area.h as f64 * area.h as f64 / content as f64) as i32).max(24).min(area.h);
    let max = (content - area.h) as f64;
    let ty = area.y + (((area.h - th) as f64) * (scroll / max).clamp(0.0, 1.0)) as i32;
    cv.fill_round(Rect::new(area.right() - 5, ty, 4, th), 2, col);
}

pub(super) fn search_box(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    r: Rect,
    field: &TextField,
    placeholder: &str,
    focused: bool,
) {
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

pub(super) fn button(cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect, label: &str, primary: bool) {
    button_h(cv, tx, th, r, label, primary, false);
}

/// [`button`] with a hover state.
pub(super) fn button_h(cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect, label: &str, primary: bool, hot: bool) {
    let c = th.chrome;
    if primary {
        cv.fill_round(r, 8, if hot { c.accent.mix(th.foreground, 0.14) } else { c.accent });
        let w = tx.ui_width(label, true);
        tx.draw_ui(cv, r.x + (r.w - w) / 2, r.y + (r.h - 18) / 2, label, on_accent(c.accent), r, true);
    } else {
        cv.card(r, 8, if hot { c.list_hover_bg } else { c.input_bg }, if hot { c.accent } else { c.border });
        let w = tx.ui_width(label, false);
        tx.draw_ui(cv, r.x + (r.w - w) / 2, r.y + (r.h - 18) / 2, label, th.foreground, r, false);
    }
}

/// Highlight (behind the text, drawn first) of what the search terms `q` matched in `s`, which is
/// drawn at (`x`, `y`).
pub(super) fn draw_hl(
    tx: &mut TextCtx,
    cv: &mut Canvas,
    th: &Theme,
    x: i32,
    y: i32,
    s: &str,
    q: &str,
    bold: bool,
    clip: Rect,
) {
    if q.trim().is_empty() {
        return;
    }
    for (a, b) in match_ranges(s, q) {
        let (Some(pre), Some(mid)) = (s.get(..a), s.get(a..b)) else { continue };
        let px = tx.ui_width(pre, bold);
        let w = tx.ui_width(mid, bold);
        cv.fill_round_clip(Rect::new(x + px - 1, y - 1, w + 2, 20), 4, th.chrome.accent.with_alpha(0x44), clip);
    }
}

/// Three-colour preview of a theme: its background with the text and accent colours as dots.
pub(super) fn swatch_chip(cv: &mut Canvas, sw: [Rgba; 3], x: i32, y: i32, border: Rgba, clip: Rect) {
    let r = Rect::new(x, y, 28, 16);
    card_c(cv, r, 5, sw[0], border, clip);
    rr(cv, Rect::new(x + 6, y + 5, 6, 6), 3, sw[1], clip);
    rr(cv, Rect::new(x + 16, y + 5, 6, 6), 3, sw[2], clip);
}

/// Width of the key-cap chips of one combo as `draw_chips` lays them out.
pub(super) fn chips_width(tx: &mut TextCtx, combo: &str) -> i32 {
    let mut w = 0;
    for part in combo_chips(combo) {
        w += (tx.ui_width(&part, false) + 14).max(22) + 4;
    }
    (w - 4).max(0)
}

/// Small pill with an icon and a word; returns its width.
pub(super) fn badge(cv: &mut Canvas, tx: &mut TextCtx, x: i32, cy: i32, text: &str, col: Rgba, clip: Rect) -> i32 {
    let tw = tx.ui_width(text, false);
    let r = Rect::new(x, cy - 10, tw + 36, 20);
    rr(cv, r, 10, col.with_alpha(0x2a), clip);
    tx.icon(cv, "ui/warning", r.x + 8, r.y + 4, 12, col, clip);
    tx.draw_ui(cv, r.x + 24, r.y + 1, text, col, r.intersect(&clip), false);
    r.w
}

/// Friendly empty state: round icon, title, hint and one primary action (a hit region).
pub(super) fn empty_state(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    mo: &Motion,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
    texts: (&str, &str, &str),
    action: Hit,
) {
    let c = th.chrome;
    let (title, hint, label) = texts;
    let cx = area.x + area.w / 2;
    let mut y = area.y + (area.h / 5).clamp(16, 56);
    rr(cv, Rect::new(cx - 24, y, 48, 48), 24, c.list_active_bg, area);
    tx.icon(cv, "hugeicons/search", cx - 12, y + 12, 24, c.tab_inactive_fg, area);
    y += 62;
    let t = fit(tx, title, area.w - 40, true);
    let w = tx.ui_width(&t, true);
    tx.draw_ui(cv, cx - w / 2, y, &t, th.foreground, area, true);
    y += 26;
    let h = fit(tx, hint, area.w - 40, false);
    let w = tx.ui_width(&h, false);
    tx.draw_ui(cv, cx - w / 2, y, &h, c.tab_inactive_fg, area, false);
    y += 34;
    let bw = tx.ui_width(label, true) + 36;
    let b = Rect::new(cx - bw / 2, y, bw, 30);
    button_h(cv, tx, th, b, label, true, mo.is_hot(&action));
    push_hit(hits, b, area, action);
}

/// The small "x" inside a search box that empties it.
pub(super) fn clear_button(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    mo: &Motion,
    field: Rect,
    hits: &mut Vec<(Rect, Hit)>,
    hit: Hit,
) {
    let c = th.chrome;
    let b = Rect::new(field.right() - 30, field.y + 5, 24, 24);
    if mo.is_hot(&hit) {
        cv.fill_round(b, 6, c.list_hover_bg);
    }
    tx.icon(
        cv,
        "hugeicons/close",
        b.x + 5,
        b.y + 5,
        14,
        if mo.is_hot(&hit) { th.foreground } else { c.tab_inactive_fg },
        b,
    );
    hits.push((b, hit));
}

/// Tooltip: a line of text and, when there is one, the shortcut as key caps.
pub(super) fn draw_tip(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    at: (i32, i32),
    text: &str,
    combo: Option<&str>,
    bounds: Rect,
) {
    let c = th.chrome;
    let tw = tx.ui_width(text, false);
    let cw = combo.map_or(0, |cb| chips_width(tx, cb) + 10);
    let (w, h) = (tw + 20 + cw, 28);
    let x = (at.0 + 12).min(bounds.right() - w - 6).max(bounds.x + 6);
    let y = if at.1 + 20 + h <= bounds.bottom() - 4 { at.1 + 20 } else { at.1 - h - 8 };
    let r = Rect::new(x, y.max(bounds.y + 4), w, h);
    cv.shadow(r, 6);
    cv.card(r, 6, c.widget_bg, c.border);
    tx.draw_ui(cv, r.x + 10, r.y + 5, text, th.foreground, r, false);
    if let Some(cb) = combo {
        draw_chips(tx, cv, th, r.x + 10 + tw + 10, r.y + h / 2, &[cb.to_string()], r.right() - 4, r, true);
    }
}

pub(super) fn draw_chips(
    tx: &mut TextCtx,
    cv: &mut Canvas,
    th: &Theme,
    x: i32,
    cy: i32,
    combos: &[String],
    max_x: i32,
    clip: Rect,
    muted: bool,
) {
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
            tx.draw_ui(
                cv,
                r.x + (w - tw) / 2,
                r.y + 1,
                &part,
                if muted { c.tab_inactive_fg } else { th.foreground },
                r.intersect(&clip),
                false,
            );
            x += w + 4;
        }
        x -= 4;
    }
}

// ───────────────────────────── settings drawing ─────────────────────────────

pub(super) fn draw_settings(
    p: &mut SettingsPage,
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
    mo: &mut Motion,
) -> Option<(Rect, i32)> {
    let c = th.chrome;
    let fg = th.foreground;
    let dim = c.tab_inactive_fg;
    let searching = p.searching();
    let query = p.search.text.trim().to_string();

    // Left column: sections.
    cv.fill(Rect::new(area.x + SIDE_W, area.y, 1, area.h - 14), c.border);
    let secs = p.sections();
    let vis = p.visible();
    // The active page's highlight and accent bar glide between entries (and fade while searching).
    let (reduce, dt) = (mo.reduce, mo.dt);
    let ny = Motion::glide(&mut mo.nav, p.section as f32, reduce, dt, &mut mo.busy);
    let na = {
        let moving = drive(&mut mo.nav_a, if searching { 0.0 } else { 1.0 }, reduce, dt);
        mo.busy |= moving;
        mo.nav_a.value.clamp(0.0, 1.0)
    };
    let ind = Rect::new(area.x + 10, area.y + 14 + (ny * 36.0).round() as i32, SIDE_W - 20, 32);
    cv.fill_round(ind, 8, fade(c.list_active_bg, na));
    rr(cv, Rect::new(ind.x + 2, ind.y + 8, 3, 16), 2, fade(c.accent, na), area);
    for (i, s) in secs.iter().enumerate() {
        let r = Rect::new(area.x + 10, area.y + 14 + i as i32 * 36, SIDE_W - 20, 32);
        let active = !searching && i == p.section;
        if !active && mo.is_hot(&Hit::Section(i)) {
            cv.fill_round(r, 8, c.list_hover_bg);
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
        } else {
            // How many rows the page has, so the size of each page is clear before opening it.
            let n = (0..p.defs.len()).filter(|&d| p.defs[d].section == *s && p.shown(d)).count();
            let t = n.to_string();
            let w = tx.ui_width(&t, false);
            tx.draw_ui(cv, r.right() - w - 10, r.y + 7, &t, dim.with_alpha(150), r, false);
            // A dot when something on the page differs from its default.
            if (0..p.defs.len()).any(|d| p.defs[d].section == *s && p.shown(d) && p.is_modified(d)) {
                cv.fill_round(Rect::new(r.right() - w - 22, r.y + 13, 6, 6), 3, c.accent);
            }
        }
        hits.push((r, Hit::Section(i)));
    }

    // Right: search + rows.
    let rx = area.x + SIDE_W + 16;
    let rw = area.w - SIDE_W - 32;
    let sb = Rect::new(rx, area.y + 12, rw, 34);
    search_box(cv, tx, th, sb, &p.search, "Search settings…", p.edit.is_none());
    hits.push((sb, Hit::Search));
    if !p.search.text.is_empty() {
        clear_button(cv, tx, th, mo, sb, hits, Hit::ClearSearch);
    }

    // Page header: title, one-line description and how many rows differ from their default.
    let hy = sb.bottom() + 14;
    let head = Rect::new(rx, hy, rw, 46);
    let secs_now = p.sections();
    let (title, desc) = if searching {
        let n = vis.len();
        (
            "Search results".to_string(),
            format!("{n} setting{} match your search, across every page.", if n == 1 { "" } else { "s" }),
        )
    } else {
        let s = secs_now.get(p.section).copied().unwrap_or("");
        (s.to_string(), section_desc(s).to_string())
    };
    let n_mod = if searching { 0 } else { vis.iter().filter(|&&d| p.is_modified(d)).count() };
    let mod_w = if n_mod > 0 {
        let t = format!("{n_mod} modified");
        let w = tx.ui_width(&t, false);
        tx.draw_ui(cv, head.right() - w, head.y + 3, &t, c.accent_text, head, false);
        w + 16
    } else {
        0
    };
    let title = fit(tx, &title, head.w - mod_w, true);
    tx.draw_ui(cv, head.x, head.y, &title, fg, head, true);
    let desc = fit(tx, &desc, head.w, false);
    tx.draw_ui(cv, head.x, head.y + 24, &desc, dim, head, false);

    let list_top = head.bottom() + 4;
    let list = Rect::new(rx - 8, list_top, rw + 16, (area.bottom() - 30 - list_top).max(0));
    p.view_h = list.h;
    p.scroll = p.scroll.clamp(0.0, p.max_scroll());
    if p.sel >= vis.len() {
        p.sel = vis.len().saturating_sub(1);
    }
    let mut drop_anchor: Option<Rect> = None;
    if vis.is_empty() {
        let title = if searching {
            format!("No settings match \u{201c}{query}\u{201d}")
        } else {
            "Nothing to show here".into()
        };
        empty_state(
            cv,
            tx,
            th,
            mo,
            list,
            hits,
            (title.as_str(), "Try a shorter or different word, or browse the pages on the left.", "Clear search"),
            Hit::ClearSearch,
        );
    }
    for (k, &d) in vis.iter().enumerate() {
        let y = list.y + k as i32 * SET_ROW - p.scroll as i32;
        if y + SET_ROW <= list.y || y >= list.bottom() {
            continue;
        }
        let row = Rect::new(list.x, y, list.w - 10, SET_ROW - 4);
        if k == p.sel {
            rr(cv, row, 10, c.list_active_bg, list);
        } else if mo.row_hot(d) {
            rr(cv, row, 10, c.list_hover_bg, list);
        }
        push_hit(hits, row, list, Hit::SetRow(d));
        let def = p.defs[d].clone();
        // The first row of a sub-heading gets its name after the label and a hairline above it.
        let group = row_group(def.key);
        let group_start = !group.1.is_empty() && (k == 0 || row_group(p.defs[vis[k - 1]].key).1 != group.1);
        if group_start && k > 0 {
            cv.fill_clip(Rect::new(row.x + 14, row.y - 2, row.w - 28, 1), c.border, list);
        }

        // Reset icon (far right) and control area.
        if p.is_modified(d) {
            // Accent bar on the row's left edge + an accent reset icon: easy to spot while scrolling.
            rr(cv, Rect::new(row.x + 3, row.y + 10, 3, row.h - 20), 2, c.accent, list);
            let reset = Rect::new(row.right() - 14 - 18, row.y + (row.h - 18) / 2, 18, 18);
            let reset_hot = mo.is_hot(&Hit::SetReset(d));
            if reset_hot {
                rr(cv, reset.inset(-4), 8, c.accent.with_alpha(0x2a), list);
            }
            tx.icon(
                cv,
                "hugeicons/reset",
                reset.x + 1,
                reset.y + 1,
                16,
                if reset_hot { fg } else { c.accent_text },
                list,
            );
            push_hit(hits, reset.inset(-4), list, Hit::SetReset(d));
        }
        let right = row.right() - 14 - 18 - 10;
        let cy = row.y + row.h / 2;
        let ctl_w = match &def.kind {
            SettingKind::Bool => 40,
            SettingKind::Number { min, max, step } => {
                if is_slider(*min, *max, *step) {
                    214
                } else {
                    114
                }
            }
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
                    let sw = if is_theme_key(def.key) { 34 } else { 0 };
                    (m + 48 + sw).clamp(130, 264)
                }
            }
        };

        // Label + help.
        let text_w = right - ctl_w - 20 - (row.x + 14);
        let label_s = fit(tx, def.label, text_w, true);
        let lw = tx.ui_width(&label_s, true);
        let clip_row = row.intersect(&list);
        if searching {
            draw_hl(tx, cv, th, row.x + 14, row.y + 9, &label_s, &query, true, clip_row);
        }
        tx.draw_ui(cv, row.x + 14, row.y + 9, &label_s, fg, clip_row, true);
        if let Some(b) = p.badges.get(def.key).filter(|_| lw + 80 < text_w) {
            let b = fit(tx, &format!("· {b}"), text_w - lw - 8, false);
            tx.draw_ui(cv, row.x + 14 + lw + 8, row.y + 9, &b, c.accent_text, clip_row, false);
        } else if !searching && lw + 80 < text_w && group_start {
            let g = fit(tx, &format!("· {}", group.1), text_w - lw - 8, false);
            tx.draw_ui(cv, row.x + 14 + lw + 8, row.y + 9, &g, dim.with_alpha(170), clip_row, false);
        } else if searching && lw + 80 < text_w {
            tx.draw_ui(
                cv,
                row.x + 14 + lw + 8,
                row.y + 9,
                &format!("· {}", def.section),
                dim.with_alpha(170),
                clip_row,
                false,
            );
        }
        let help_s = fit(tx, def.help, text_w, false);
        if searching {
            draw_hl(tx, cv, th, row.x + 14, row.y + 29, &help_s, &query, false, clip_row);
        }
        tx.draw_ui(cv, row.x + 14, row.y + 29, &help_s, dim, clip_row, false);

        // Control.
        match &def.kind {
            SettingKind::Bool => {
                let on = p.cur_bool(d);
                let t = mo.toggle(d, on).clamp(0.0, 1.0);
                let pill = Rect::new(right - 40, cy - 11, 40, 22);
                rr(cv, pill, 11, lerp_rgba(c.border.mix(fg, 0.25), c.accent, t), list);
                if mo.is_hot(&Hit::Toggle(d)) {
                    rr(cv, pill, 11, fg.with_alpha(0x18), list);
                }
                let kx = lerp(pill.x as f32 + 3.0, pill.right() as f32 - 19.0, t).round() as i32;
                rr(
                    cv,
                    Rect::new(kx, pill.y + 3, 16, 16),
                    8,
                    lerp_rgba(Rgba(250, 250, 250, 255), on_accent(c.accent), t),
                    list,
                );
                push_hit(hits, pill, list, Hit::Toggle(d));
            }
            SettingKind::Choice(_) => {
                let opts = p.choices_of(d);
                let cur = p.cur_str(d);
                if is_segmented(opts.len()) {
                    let bar = Rect::new(right - ctl_w, cy - 15, ctl_w, 30);
                    card_c(cv, bar, 8, c.input_bg, c.border, list);
                    let mut spans: Vec<(i32, i32)> = Vec::new();
                    let mut x = bar.x + 2;
                    for o in &opts {
                        let w = tx.ui_width(&choice_label(o), false) + 22;
                        spans.push((x, w));
                        x += w;
                    }
                    // The accent pill slides to the chosen option.
                    let known = opts.iter().position(|o| *o == cur);
                    let f = mo.seg(d, known.unwrap_or(0));
                    if known.is_some()
                        && let Some((sx, sw)) = seg_span(&spans, f)
                    {
                        rr(cv, Rect::new(sx, bar.y + 2, sw, bar.h - 4), 6, c.accent, list);
                    }
                    for (oi, o) in opts.iter().enumerate() {
                        let label = choice_label(o);
                        let (sx, w) = spans[oi];
                        let seg = Rect::new(sx, bar.y + 2, w, bar.h - 4);
                        let active = *o == cur;
                        let under_pill = known.is_some() && (f - oi as f32).abs() < 0.5;
                        if !active && mo.is_hot(&Hit::Seg(d, oi)) {
                            rr(cv, seg, 6, c.list_hover_bg, list);
                        }
                        tx.draw_ui(
                            cv,
                            seg.x + 11,
                            seg.y + 5,
                            &label,
                            if under_pill { on_accent(c.accent) } else { fg },
                            seg.intersect(&list),
                            active,
                        );
                        push_hit(hits, seg, list, Hit::Seg(d, oi));
                    }
                } else {
                    let btn = Rect::new(right - ctl_w, cy - 15, ctl_w, 30);
                    let open = p.drop.as_ref().is_some_and(|x| x.def == d);
                    let hot = mo.is_hot(&Hit::DropOpen(d));
                    card_c(
                        cv,
                        btn,
                        8,
                        if hot && !open { c.list_hover_bg } else { c.input_bg },
                        if open { c.accent } else { c.border },
                        list,
                    );
                    let mut lx = btn.x + 12;
                    if is_theme_key(def.key)
                        && let Some(sw) = mo.swatch(&cur)
                    {
                        swatch_chip(cv, sw, lx, btn.y + (btn.h - 16) / 2, c.border, btn.intersect(&list));
                        lx += 34;
                    }
                    let label = if cur.is_empty() { "—".to_string() } else { choice_label(&cur) };
                    let label = fit(tx, &label, btn.right() - 32 - lx, false);
                    tx.draw_ui(cv, lx, btn.y + 6, &label, fg, btn.intersect(&list), false);
                    tx.icon(cv, "hugeicons/chevron_down", btn.right() - 26, btn.y + 7, 16, dim, btn.intersect(&list));
                    push_hit(hits, btn, list, Hit::DropOpen(d));
                    if open {
                        drop_anchor = Some(btn);
                    }
                }
            }
            SettingKind::Number { min, max, step } if is_slider(*min, *max, *step) => {
                let v = p.cur_num(d);
                let chip = Rect::new(right - 58, cy - 12, 58, 24);
                let track = Rect::new(right - ctl_w + 7, cy - 2, ctl_w - 58 - 22, 4);
                rr(cv, track, 2, c.border.mix(fg, 0.25), list);
                let fill_w = (track.w as f32 * slider_frac(*min, *max, v)).round() as i32;
                if fill_w > 0 {
                    rr(cv, Rect::new(track.x, track.y, fill_w, 4), 2, c.accent, list);
                }
                let hot = mo.is_hot(&Hit::Slide(d)) || mo.drag == Some(d);
                let knob = Rect::new(track.x + fill_w - 7, cy - 7, 14, 14);
                if hot {
                    rr(cv, knob.inset(-4), 11, c.accent.with_alpha(0x30), list);
                }
                card_c(cv, knob, 7, Rgba(250, 250, 250, 255), c.accent, list);
                card_c(cv, chip, 6, c.input_bg, c.border, list);
                let t = if def.key == "ui_scale" && v == 0.0 { "Auto".to_string() } else { fmt_num(v) };
                let w = tx.ui_width(&t, true);
                tx.draw_ui(cv, chip.x + (chip.w - w) / 2, chip.y + 3, &t, fg, chip.intersect(&list), true);
                mo.tracks.insert(d, track);
                push_hit(hits, Rect::new(track.x - 8, cy - 14, track.w + 16, 28), list, Hit::Slide(d));
            }
            SettingKind::Number { min, max, .. } => {
                let v = p.cur_num(d);
                let x0 = right - ctl_w;
                for (i, (dir, icon, enabled)) in
                    [(-1, "minus", v > *min), (1, "plus", v < *max)].into_iter().enumerate()
                {
                    let bx = if i == 0 { x0 } else { right - 30 };
                    let b = Rect::new(bx, cy - 15, 30, 30);
                    let hot = enabled && mo.is_hot(&Hit::Step(d, dir));
                    card_c(cv, b, 8, if hot { c.list_hover_bg } else { c.input_bg }, c.border, list);
                    tx.icon(
                        cv,
                        &format!("hugeicons/{icon}"),
                        b.x + 7,
                        b.y + 7,
                        16,
                        if enabled { fg } else { dim.with_alpha(90) },
                        b.intersect(&list),
                    );
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
                let hot = mo.is_hot(&Hit::EditText(d));
                let edge = if editing.is_some() {
                    c.accent
                } else if hot {
                    c.border.mix(fg, 0.35)
                } else {
                    c.border
                };
                card_c(cv, b, 8, c.input_bg, edge, list);
                let inner = Rect::new(b.x + 10, b.y + 1, b.w - 20, b.h - 2).intersect(&list);
                match editing {
                    Some((_, f)) => {
                        if f.sel_all {
                            let w = tx.ui_width(&f.text, false);
                            cv.fill_round_clip(
                                Rect::new(b.x + 7, b.y + 3, w + 6, 24),
                                5,
                                c.accent.with_alpha(0x55),
                                inner,
                            );
                        }
                        tx.draw_ui(cv, b.x + 10, b.y + 6, &f.text, fg, inner, false);
                        let prefix: String = f.text.chars().take(f.cursor).collect();
                        let cx = b.x + 10 + tx.ui_width(&prefix, false);
                        cv.fill_clip(Rect::new(cx, b.y + 6, 1, 18), fg, inner);
                    }
                    None => {
                        let s = p.cur_str(d);
                        if s.is_empty() {
                            let empty = if def.key.ends_with("_font_family") { "System default" } else { "—" };
                            tx.draw_ui(cv, b.x + 10, b.y + 6, empty, dim, inner, false);
                        } else {
                            tx.draw_ui(cv, b.x + 10, b.y + 6, &s, fg, inner, false);
                        }
                    }
                }
                push_hit(hits, b, list, Hit::EditText(d));
            }
        }
    }
    // The list's scrollbar is the app's draggable `ui::VBar` (see `Modal::bar_info`).
    let list_info = (list, vis.len() as i32 * SET_ROW);

    // Dropdown popup, above everything else.
    if let (Some(a), Some(dr)) = (drop_anchor, p.drop.clone()) {
        let opts = p.choices_of(dr.def);
        let cur = p.cur_str(dr.def);
        let n = opts.len().min(DROP_VIS);
        let ph = n as i32 * 26 + 8;
        let themed = is_theme_key(p.defs[dr.def].key);
        let pad = if themed { 34 } else { 0 };
        let mut maxw = a.w;
        for o in opts.iter().skip(dr.scroll).take(DROP_VIS) {
            maxw = maxw.max(tx.ui_width(&choice_label(o), false) + 40 + pad);
        }
        let pw = maxw.min(area.w - SIDE_W - 20);
        let py = if a.bottom() + 4 + ph <= area.bottom() - 6 { a.bottom() + 4 } else { (a.y - 4 - ph).max(area.y + 4) };
        let pr = Rect::new(a.right() - pw, py, pw, ph);
        cv.shadow(pr, 8);
        cv.card(pr, 8, c.widget_bg, c.border);
        hits.push((pr, Hit::Block));
        for (i, o) in opts.iter().enumerate().skip(dr.scroll).take(DROP_VIS) {
            let ir = Rect::new(pr.x + 4, pr.y + 4 + (i - dr.scroll) as i32 * 26, pr.w - 8, 26);
            if i == dr.sel {
                cv.fill_round(ir, 6, c.list_active_bg);
            } else if mo.is_hot(&Hit::DropItem(i)) {
                cv.fill_round(ir, 6, c.list_hover_bg);
            }
            let mut lx = ir.x + 10;
            if themed && let Some(sw) = mo.swatch(o) {
                swatch_chip(cv, sw, lx, ir.y + 5, c.border, ir);
            }
            if themed {
                lx += 34;
            }
            let label = fit(tx, &choice_label(o), ir.right() - 30 - lx, false);
            tx.draw_ui(cv, lx, ir.y + 4, &label, fg, ir, false);
            if *o == cur {
                tx.icon(cv, "hugeicons/check", ir.right() - 24, ir.y + 5, 16, c.accent, ir);
            }
            hits.push((ir, Hit::DropItem(i)));
        }
        scrollbar(
            cv,
            Rect::new(pr.x, pr.y + 4, pr.w - 2, ph - 8),
            dr.scroll as f64,
            opts.len() as i32 * 26,
            dim.with_alpha(120),
        );
    }

    // Footer hint.
    let hint =
        p.footer.as_deref().unwrap_or("↑↓ navigate   Enter/Space change   ←→ adjust   Tab switch section   Esc close");
    let hint = fit(tx, hint, rw, false);
    tx.draw_ui(cv, rx, area.bottom() - 26, &hint, dim.with_alpha(170), area, false);
    Some(list_info)
}

// ───────────────────────────── keybindings drawing ─────────────────────────────

pub(super) fn draw_keybinds(
    p: &mut KeybindsPage,
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
    mo: &mut Motion,
) -> Option<(Rect, i32)> {
    let c = th.chrome;
    let fg = th.foreground;
    let dim = c.tab_inactive_fg;
    let pad = 20;
    let query = p.filter.text.trim().to_string();

    let btn_label = "Restore defaults";
    let bw = tx.ui_width(btn_label, false) + 52;
    let btn = Rect::new(area.right() - pad - bw, area.y + 12, bw, 34);
    let btn_hot = mo.is_hot(&Hit::KbResetAll);
    cv.card(btn, 8, if btn_hot { c.list_hover_bg } else { c.input_bg }, c.border);
    tx.icon(cv, "hugeicons/reset", btn.x + 12, btn.y + 9, 16, if btn_hot { fg } else { dim }, btn);
    tx.draw_ui(cv, btn.x + 36, btn.y + 8, btn_label, fg, btn, false);
    hits.push((btn, Hit::KbResetAll));
    let fr = Rect::new(area.x + pad, area.y + 12, btn.x - 10 - (area.x + pad), 34);
    search_box(cv, tx, th, fr, &p.filter, "Filter commands or shortcuts…", p.mode == Mode::Browse);
    hits.push((fr, Hit::Search));
    if !p.filter.text.is_empty() {
        clear_button(cv, tx, th, mo, fr, hits, Hit::KbClearFilter);
    }

    // Column captions.
    let hdr_y = fr.bottom() + 8;
    let list =
        Rect::new(area.x + pad - 8, hdr_y + 24, area.w - 2 * pad + 16, (area.bottom() - FOOT_H - (hdr_y + 24)).max(0));
    let row_x = list.x + 12;
    let tw = list.w - 10 - 24 - 30;
    let (c2, c3) = (row_x + tw * 40 / 100, row_x + tw * 76 / 100);
    for (x, t) in [(row_x, "Command"), (c2, "Shortcut"), (c3, "Default")] {
        tx.draw_ui(cv, x, hdr_y + 2, t, dim, area, true);
    }
    cv.fill(Rect::new(area.x + pad - 8, hdr_y + 23, list.w, 1), c.border);

    p.view_h = list.h;
    p.scroll = p.scroll.clamp(0.0, p.max_scroll());
    if p.sel >= p.filtered.len() {
        p.sel = p.filtered.len().saturating_sub(1);
    }
    if p.filtered.is_empty() {
        let title = format!("No commands match \u{201c}{query}\u{201d}");
        empty_state(
            cv,
            tx,
            th,
            mo,
            list,
            hits,
            (
                title.as_str(),
                "Search by command name or by the keys it uses, for example \u{201c}ctrl s\u{201d}.",
                "Clear filter",
            ),
            Hit::KbClearFilter,
        );
    }
    // The row being recorded pulses its ring (steady when motion is reduced).
    if p.mode == Mode::Record && !mo.reduce {
        mo.pulse += mo.dt as f32 * 4.0;
        mo.busy = true;
    }
    let pulse = if mo.reduce { 1.0 } else { 0.5 + 0.5 * mo.pulse.sin() };
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
        let in_use = matches!(&p.mode, Mode::Confirm { other, .. } if *other == cmd.id);
        if selected {
            rr(cv, row, 8, c.list_active_bg, list);
        } else if mo.is_hot(&Hit::KbRow(k)) {
            rr(cv, row, 8, c.list_hover_bg, list);
        }
        if in_use {
            rr(cv, Rect::new(row.x + 2, row.y + 6, 3, row.h - 12), 2, c.warning, list);
        }
        push_hit(hits, row, list, Hit::KbRow(k));
        let cy = row.y + row.h / 2;
        let label = fit(tx, &cmd.label, c2 - row_x - 12, false);
        tx.draw_ui(cv, row_x, cy - 9, &label, fg, clip, false);

        // Current binding(s).
        let recording = selected && p.mode != Mode::Browse && p.mode != Mode::ConfirmReset;
        if recording {
            match &p.mode {
                Mode::Confirm { combo, .. } => {
                    draw_chips(tx, cv, th, c2, cy, std::slice::from_ref(combo), c3 - 8, clip, false);
                    let bx = c2 + chips_width(tx, combo) + 10;
                    if bx + 80 < c3 {
                        badge(cv, tx, bx, cy, "Conflict", c.warning, clip);
                    }
                }
                _ => {
                    let cell = Rect::new(c2 - 6, cy - 13, c3 - c2 - 4, 26);
                    let ring = lerp_rgba(c.border, c.accent, 0.35 + 0.65 * pulse);
                    card_c(cv, cell, 6, c.input_bg, ring, clip);
                    rr(cv, Rect::new(cell.x + 9, cy - 3, 6, 6), 3, fade(c.accent, 0.35 + 0.65 * pulse), clip);
                    let t = fit(tx, "Press keys…", cell.w - 34, false);
                    tx.draw_ui(cv, cell.x + 22, cell.y + 4, &t, c.accent_text, cell.intersect(&clip), false);
                }
            }
        } else {
            let cur = p.bindings_of(&cmd.id);
            if cur.is_empty() {
                tx.draw_ui(cv, c2, cy - 9, "—", dim, clip, false);
            } else {
                draw_chips(tx, cv, th, c2, cy, &cur, c3 - 8, clip, false);
            }
            if in_use {
                let used: i32 = cur.iter().map(|b| chips_width(tx, b) + 12).sum();
                if c2 + used + 80 < c3 {
                    badge(cv, tx, c2 + used, cy, "In use", c.warning, clip);
                }
            }
        }
        // Default.
        match &cmd.default {
            Some(d) => draw_chips(tx, cv, th, c3, cy, std::slice::from_ref(d), row.right() - 40, clip, true),
            None => tx.draw_ui(cv, c3, cy - 9, "—", dim.with_alpha(120), clip, false),
        }
        if p.is_modified(&cmd.id) {
            let ic = Rect::new(row.right() - 14 - 18, cy - 9, 18, 18);
            let ic_hot = mo.is_hot(&Hit::KbReset(k));
            if ic_hot {
                rr(cv, ic.inset(-4), 8, c.accent.with_alpha(0x2a), list);
            }
            tx.icon(cv, "hugeicons/reset", ic.x + 1, ic.y + 1, 16, if ic_hot { fg } else { dim }, clip);
            push_hit(hits, ic.inset(-5), list, Hit::KbReset(k));
        }
    }
    let list_info = (list, p.filtered.len() as i32 * KB_ROW);

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
                None => (
                    "Enter record shortcut   Del remove   Ctrl+R restore row   Ctrl+Shift+R restore all   Esc close"
                        .to_string(),
                    dim.with_alpha(170),
                ),
            };
            let text = fit(tx, &text, area.w - 2 * pad, false);
            tx.draw_ui(cv, left, ty, &text, col, area, false);
        }
        Mode::Record => {
            let name = p.selected_id().map(|i| p.label_of(i)).unwrap_or_default();
            let (text, col) = match &p.note {
                Some(m) => (m.clone(), c.warning),
                None => (format!("Press the new shortcut for «{name}»…   Esc cancels"), c.accent),
            };
            let text = fit(tx, &text, area.w - 2 * pad, false);
            tx.draw_ui(cv, left, ty, &text, col, area, false);
        }
        Mode::Confirm { combo, other } => {
            let chips = combo_chips(&combo).join("+");
            let text = format!("{chips} is already used by «{}».", p.label_of(&other));
            let (b1w, b2w) = (96, 90);
            let b2 = Rect::new(area.right() - pad - b2w, btn_y, b2w, btn_h);
            let b1 = Rect::new(b2.x - 8 - b1w, btn_y, b1w, btn_h);
            let text = fit(tx, &text, b1.x - left - 12, false);
            tx.draw_ui(cv, left, ty, &text, c.warning, area, false);
            button_h(cv, tx, th, b1, "Replace", true, mo.is_hot(&Hit::KbConfirm));
            button_h(cv, tx, th, b2, "Cancel", false, mo.is_hot(&Hit::KbCancel));
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
        Mode::ConfirmReset => {
            let (b1w, b2w) = (96, 90);
            let b2 = Rect::new(area.right() - pad - b2w, btn_y, b2w, btn_h);
            let b1 = Rect::new(b2.x - 8 - b1w, btn_y, b1w, btn_h);
            let text = fit(tx, "Restore all shortcuts to their defaults?", b1.x - left - 12, false);
            tx.draw_ui(cv, left, ty, &text, c.warning, area, false);
            button_h(cv, tx, th, b1, "Restore", true, mo.is_hot(&Hit::KbConfirm));
            button_h(cv, tx, th, b2, "Cancel", false, mo.is_hot(&Hit::KbCancel));
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
    }
    Some(list_info)
}
