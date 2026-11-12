use super::*;

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
) -> Option<(Rect, i32)> {
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
    search_box(cv, tx, th, sb, &p.search, "Search settings…", p.edit.is_none());
    hits.push((sb, Hit::Search));

    let list = Rect::new(rx - 8, sb.bottom() + 10, rw + 16, (area.bottom() - 30 - (sb.bottom() + 10)).max(0));
    p.view_h = list.h;
    p.scroll = p.scroll.clamp(0.0, p.max_scroll());
    if p.sel >= vis.len() {
        p.sel = vis.len().saturating_sub(1);
    }
    let mut drop_anchor: Option<Rect> = None;
    if vis.is_empty() {
        let msg = "No settings found";
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
        if let Some(b) = p.badges.get(def.key).filter(|_| lw + 80 < text_w) {
            let b = fit(tx, &format!("· {b}"), text_w - lw - 8, false);
            tx.draw_ui(cv, row.x + 14 + lw + 8, row.y + 9, &b, c.accent_text, clip_row, false);
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
        tx.draw_ui(cv, row.x + 14, row.y + 29, &help_s, dim, clip_row, false);

        // Control.
        match &def.kind {
            SettingKind::Bool => {
                let on = p.cur_bool(d);
                let pill = Rect::new(right - 40, cy - 11, 40, 22);
                rr(cv, pill, 11, if on { c.accent } else { c.border.mix(fg, 0.25) }, list);
                let kx = if on { pill.right() - 19 } else { pill.x + 3 };
                rr(
                    cv,
                    Rect::new(kx, pill.y + 3, 16, 16),
                    8,
                    if on { on_accent(c.accent) } else { Rgba(250, 250, 250, 255) },
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
                    let mut x = bar.x + 2;
                    for (oi, o) in opts.iter().enumerate() {
                        let label = choice_label(o);
                        let w = tx.ui_width(&label, false) + 22;
                        let seg = Rect::new(x, bar.y + 2, w, bar.h - 4);
                        let active = *o == cur;
                        if active {
                            rr(cv, seg, 6, c.accent, list);
                        }
                        tx.draw_ui(
                            cv,
                            seg.x + 11,
                            seg.y + 5,
                            &label,
                            if active { on_accent(c.accent) } else { fg },
                            seg.intersect(&list),
                            active,
                        );
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
                for (i, (dir, icon, enabled)) in
                    [(-1, "minus", v > *min), (1, "plus", v < *max)].into_iter().enumerate()
                {
                    let bx = if i == 0 { x0 } else { right - 30 };
                    let b = Rect::new(bx, cy - 15, 30, 30);
                    card_c(cv, b, 8, c.input_bg, c.border, list);
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
                card_c(cv, b, 8, c.input_bg, if editing.is_some() { c.accent } else { c.border }, list);
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
) -> Option<(Rect, i32)> {
    let c = th.chrome;
    let fg = th.foreground;
    let dim = c.tab_inactive_fg;
    let pad = 20;

    let btn_label = "Restore defaults";
    let bw = tx.ui_width(btn_label, false) + 52;
    let btn = Rect::new(area.right() - pad - bw, area.y + 12, bw, 34);
    cv.card(btn, 8, c.input_bg, c.border);
    tx.icon(cv, "hugeicons/reset", btn.x + 12, btn.y + 9, 16, dim, btn);
    tx.draw_ui(cv, btn.x + 36, btn.y + 8, btn_label, fg, btn, false);
    hits.push((btn, Hit::KbResetAll));
    let fr = Rect::new(area.x + pad, area.y + 12, btn.x - 10 - (area.x + pad), 34);
    search_box(cv, tx, th, fr, &p.filter, "Filter commands or shortcuts…", p.mode == Mode::Browse);
    hits.push((fr, Hit::Search));

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
        let msg = "No commands found";
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
                Mode::Confirm { combo, .. } => {
                    draw_chips(tx, cv, th, c2, cy, std::slice::from_ref(combo), c3 - 8, clip, false)
                }
                _ => {
                    let cell = Rect::new(c2 - 6, cy - 13, c3 - c2 - 4, 26);
                    card_c(cv, cell, 6, c.input_bg, c.accent, clip);
                    let t = fit(tx, "Press the new shortcut…", cell.w - 16, false);
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
            button(cv, tx, th, b1, "Replace", true);
            button(cv, tx, th, b2, "Cancel", false);
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
        Mode::ConfirmReset => {
            let (b1w, b2w) = (96, 90);
            let b2 = Rect::new(area.right() - pad - b2w, btn_y, b2w, btn_h);
            let b1 = Rect::new(b2.x - 8 - b1w, btn_y, b1w, btn_h);
            let text = fit(tx, "Restore all shortcuts to their defaults?", b1.x - left - 12, false);
            tx.draw_ui(cv, left, ty, &text, c.warning, area, false);
            button(cv, tx, th, b1, "Restore", true);
            button(cv, tx, th, b2, "Cancel", false);
            hits.push((b1, Hit::KbConfirm));
            hits.push((b2, Hit::KbCancel));
        }
    }
    Some(list_info)
}
