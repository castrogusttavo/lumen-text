use super::*;

impl App {
    // ───────────── drawing ─────────────

    pub fn draw_find(&mut self, cv: &mut Canvas) {
        self.refresh_find();
        let Some(f) = &self.find else { return };
        let c = self.theme.chrome;
        let ed = self.layout.editor;
        let w = (ed.w - 40).clamp(280, 540);
        let h = if f.show_replace { 78 } else { 42 };
        let r = Rect::new(ed.right() - w - 14, ed.y + 8, w, h);
        cv.shadow(r, 10);
        cv.card(r, 10, c.widget_bg, c.border);
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let field_w = w - 168;
        let field = |cv: &mut Canvas,
                     tx: &mut crate::ui::TextCtx,
                     y: i32,
                     text: &str,
                     hint: &str,
                     cursor: usize,
                     focus: bool,
                     bad: bool,
                     selall: bool| {
            let b = Rect::new(r.x + 8, y, field_w, 28);
            cv.card(b, 7, c.input_bg, if focus { c.accent } else { c.border });
            let (shown, col) = if text.is_empty() { (hint, dim) } else { (text, if bad { c.error } else { fg }) };
            if selall {
                let w = tx.ui_width(text, false);
                cv.fill_round(Rect::new(b.x + 7, b.y + 3, w + 4, 22), 4, c.accent.with_alpha(0x50));
            }
            tx.draw_ui(cv, b.x + 9, b.y + 5, shown, col, b.inset(1), false);
            if focus {
                let prefix: String = text.chars().take(cursor).collect();
                let x = b.x + 9 + tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, b.y + 6, 1, 16), fg, b);
            }
        };
        field(
            cv,
            &mut self.tx,
            r.y + 7,
            &f.field.text,
            "Search",
            f.field.cursor,
            f.focused && !f.in_replace,
            !f.valid,
            f.field.sel_all,
        );
        let count = match (f.matches.len(), f.current) {
            (0, _) => {
                if f.field.text.is_empty() {
                    String::new()
                } else {
                    "no results".to_string()
                }
            }
            (n, Some(c)) => format!("{} de {}", c + 1, n),
            (n, None) => format!("{n}"),
        };
        let mut x = r.x + 8 + field_w + 10;
        let ty = r.y + 7 + 5;
        let toggle = |on: bool| if on { c.accent } else { dim };
        self.tx.draw_ui(cv, x, ty, "Aa", toggle(f.case), r, true);
        x += 28;
        self.tx.draw_ui(cv, x, ty, ".*", toggle(f.regex), r, true);
        x += 26;
        self.tx.draw_ui(cv, x, ty, &count, dim, r, false);
        if f.show_replace {
            field(
                cv,
                &mut self.tx,
                r.y + 43,
                &f.replace.text,
                "Replace",
                f.replace.cursor,
                f.focused && f.in_replace,
                false,
                f.replace.sel_all,
            );
            self.tx.draw_ui(cv, r.x + 8 + field_w + 10, r.y + 43 + 5, "Enter · Ctrl+Alt+Enter all", dim, r, false);
        }
    }

    pub fn draw_overlay(&mut self, cv: &mut Canvas) {
        self.layout.overlay_rows.clear();
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let base = self.picker_rect();
        self.regions.modal_from = self.regions.list.len();
        // (list area, total rows, scroll row) of the picker: its scrollbar is added after the borrow ends.
        let mut picker_bar: Option<(Rect, f64, f64)> = None;

        match &self.overlay {
            Overlay::None => {}
            Overlay::Settings(_) => self.draw_settings(cv),
            Overlay::TopMenu(_) => self.draw_topmenu(cv),
            Overlay::Ctx(_) => self.draw_ctxmenu(cv),
            Overlay::Prompt(p) => {
                let r = Rect::new(base.x, base.y, base.w, 86);
                cv.shadow(r, 12);
                cv.card(r, 12, c.widget_bg, c.border);
                self.tx.draw_ui(cv, r.x + 16, r.y + 12, &p.title, dim, r, false);
                let input = Rect::new(r.x + 12, r.y + 38, r.w - 24, 34);
                cv.card(input, 8, c.input_bg, c.accent);
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(Rect::new(input.x + 8, input.y + 6, w + 4, 22), 4, c.accent.with_alpha(0x50));
                }
                self.tx.draw_ui(cv, input.x + 10, input.y + 8, &p.field.text, fg, input.inset(1), false);
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = input.x + 10 + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, input.y + 8, 1, 18), fg, input);
            }
            Overlay::Picker(p) => {
                let shown = p.filtered.len().min(PICKER_ROWS);
                let list_h = if shown == 0 { 44 } else { shown as i32 * ROW + 14 };
                let r = Rect::new(base.x, base.y, base.w, INPUT_H + list_h);
                cv.shadow(r, 16);
                cv.card(r, 16, c.widget_bg, c.border);
                // Input row.
                self.tx.icon(cv, "hugeicons/search", r.x + 18, r.y + (INPUT_H - 16) / 2, 16, dim, r);
                let (shown_text, col) =
                    if p.field.text.is_empty() { (p.title.as_str(), dim) } else { (p.field.text.as_str(), fg) };
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(
                        Rect::new(r.x + 44, r.y + (INPUT_H - 24) / 2, w + 4, 24),
                        4,
                        c.accent.with_alpha(0x50),
                    );
                }
                self.tx.draw_ui(cv, r.x + 46, r.y + (INPUT_H - 18) / 2, shown_text, col, r, false);
                if p.kind == PickKind::ProjectSearch {
                    let tag = format!("{}{}", if p.case { "Aa  " } else { "" }, if p.regex { ".*" } else { "" });
                    let tw = self.tx.ui_width(&tag, true);
                    self.tx.draw_ui(cv, r.right() - tw - 16, r.y + (INPUT_H - 18) / 2, &tag, c.accent_text, r, true);
                }
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = r.x + 46 + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, r.y + (INPUT_H - 20) / 2, 1, 20), fg, r);
                cv.fill(Rect::new(r.x + 16, r.y + INPUT_H - 1, r.w - 32, 1), c.border);

                let mut rows = Vec::new();
                // Mid-glide the list sits a fraction of a row past `p.scroll`: rows shift up by `off`
                // px, one more row shows at the bottom and everything is clipped to the list.
                let off = self.region_off(crate::scrollreg::Reg::Picker, ROW);
                let lclip = Rect::new(r.x, r.y + INPUT_H, r.w, list_h);
                for (k, (idx, pos)) in
                    p.filtered.iter().enumerate().skip(p.scroll).take(PICKER_ROWS + (off > 0) as usize)
                {
                    let it = &p.items[*idx];
                    let y = r.y + INPUT_H + 6 + (k - p.scroll) as i32 * ROW - off;
                    let row_full = Rect::new(r.x + 8, y, r.w - 16, ROW);
                    let rr = row_full.intersect(&lclip);
                    if k == p.sel {
                        cv.fill_round_clip(row_full, 10, c.list_active_bg, lclip);
                    }
                    let avail = rr.w - 28;
                    let detail_w = if matches!(p.kind, PickKind::Commands | PickKind::Themes | PickKind::Choice)
                        && !it.detail.is_empty()
                    {
                        self.tx.ui_width(&it.detail, false) + 20
                    } else {
                        0
                    };
                    // Label, shortened from the left when it does not fit (paths), matches highlighted.
                    let label: Vec<char> = it.label.chars().collect();
                    let mut skip = 0;
                    let budget = avail - detail_w;
                    let max_label =
                        if it.detail.is_empty() || p.kind == PickKind::Commands { budget } else { budget * 3 / 5 };
                    while skip < label.len()
                        && self.tx.ui_width(&label[skip..].iter().collect::<String>(), false) > max_label
                    {
                        skip += (label.len() - skip).div_ceil(8).max(1);
                    }
                    let dir_end = if p.kind == PickKind::QuickOpen {
                        label.iter().rposition(|ch| *ch == '/').map_or(0, |i| i + 1)
                    } else {
                        0
                    };
                    // Match colour: the accent when it has a hue of its own, else the plain text colour.
                    let chroma = c.accent_text.0.max(c.accent_text.1).max(c.accent_text.2)
                        - c.accent_text.0.min(c.accent_text.1).min(c.accent_text.2);
                    let mcol = if chroma >= 40 { c.accent_text } else { fg };
                    let mut pieces: Vec<(String, crate::theme::Rgba)> = Vec::new();
                    if skip > 0 {
                        pieces.push(("…".to_string(), dim));
                    }
                    for (i, ch) in label.iter().enumerate().skip(skip) {
                        let col = if pos.contains(&i) {
                            mcol
                        } else if i < dir_end {
                            dim
                        } else {
                            fg
                        };
                        match pieces.last_mut() {
                            Some((s, pc)) if *pc == col => s.push(*ch),
                            _ => pieces.push((ch.to_string(), col)),
                        }
                    }
                    let ly = y + (ROW - 18) / 2;
                    self.tx.draw_ui_pieces(cv, rr.x + 14, ly, &pieces, rr, false);
                    // Matched letters: an underline in the text colour on top of the accent colour, so
                    // they stand out on the selected / hovered row without relying on colour alone.
                    let mut i = skip;
                    while i < label.len() {
                        if !pos.contains(&i) {
                            i += 1;
                            continue;
                        }
                        let mut j = i;
                        while j < label.len() && pos.contains(&j) {
                            j += 1;
                        }
                        let pre: String = std::iter::repeat_n('…', (skip > 0) as usize)
                            .chain(label[skip..i].iter().copied())
                            .collect();
                        let run: String = label[i..j].iter().collect();
                        let x0 = rr.x + 14 + self.tx.ui_width(&pre, false);
                        cv.fill_clip(Rect::new(x0, ly + 17, self.tx.ui_width(&run, false), 1), mcol, rr);
                        i = j;
                    }
                    if !it.detail.is_empty() {
                        if p.kind == PickKind::Commands {
                            let dx = rr.right() - detail_w + 8;
                            self.tx.draw_ui(cv, dx, ly, &it.detail, dim, rr, false);
                        } else {
                            let used =
                                self.tx.ui_width(&pieces.iter().map(|(s, _)| s.as_str()).collect::<String>(), false);
                            let dx = rr.x + 14 + used + 16;
                            let room = rr.right() - dx - 10;
                            let mut det = it.detail.clone();
                            while self.tx.ui_width(&det, false) > room && det.chars().count() > 3 {
                                det.remove(0);
                            }
                            self.tx.draw_ui(cv, dx, ly, &det, dim, rr, false);
                        }
                    }
                    rows.push((rr, k));
                }
                self.layout.overlay_rows = rows;
                if p.filtered.len() > PICKER_ROWS {
                    picker_bar =
                        Some((Rect::new(r.x, r.y + INPUT_H, r.w, list_h), p.filtered.len() as f64, p.scroll as f64));
                }
                if p.filtered.is_empty() {
                    let msg = if p.kind == PickKind::ProjectSearch && p.field.text.is_empty() {
                        "Type to search the project"
                    } else {
                        "No results"
                    };
                    self.tx.draw_ui(cv, r.x + 16, r.y + INPUT_H + 10, msg, dim, r, false);
                }
            }
        }
        if let Some((area, total, pos)) = picker_bar {
            self.region_bar(
                cv,
                crate::scrollreg::Reg::Picker,
                area.inset(0),
                total,
                PICKER_ROWS as f64,
                pos,
                ROW as f64,
                true,
            );
        }
    }
}
