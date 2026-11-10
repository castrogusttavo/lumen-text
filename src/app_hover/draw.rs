use super::*;

impl App {
    // ───────────── drawing ─────────────

    pub fn draw_hover(&mut self, cv: &mut Canvas) {
        let Some(mut h) = self.hover.take() else { return };
        if h.is_empty() {
            return;
        }
        self.draw_tip(cv, &mut h);
        self.hover = Some(h);
    }

    pub(super) fn draw_tip(&mut self, cv: &mut Canvas, h: &mut HoverTip) {
        let chrome = self.theme.chrome;
        let ed = self.layout.editor.intersect(&cv.bounds());
        let maxw = (ed.w - 40).clamp(240, 640);
        let inner_w = maxw - 2 * PAD_X;
        let key: LayKey =
            (inner_w, font_size().to_bits(), self.tx.cell_w.to_bits(), crate::ui::scale().to_bits(), h.blocks_ver);
        if h.laid.as_ref().is_none_or(|(k, _)| *k != key) {
            let lang_blocks = h.blocks.clone();
            let App { tx, theme, tokenizers, assets, .. } = &mut *self;
            let pal = Pal::new(theme);
            let mut hl = |lang: &str, text: &str| -> Option<Vec<Vec<(usize, usize, Rgba, u8)>>> {
                let scope = scope_for(lang)?;
                let tk = match tokenizers.get(scope) {
                    Some(t) => t.clone(),
                    None => {
                        let t = crate::highlight::make_tokenizer(assets, scope)?;
                        tokenizers.insert(scope.to_string(), t.clone());
                        t
                    }
                };
                Some(crate::highlight::highlight_text(&tk, theme, text))
            };
            let mut env = Env::new(tx, pal, &mut hl);
            let laid = layout(&lang_blocks, inner_w, &mut env);
            h.laid = Some((key, laid));
        }
        let pal = Pal::new(&self.theme);
        // footer
        let acts = h.actions();
        let mut widths: Vec<i32> = acts.iter().map(|a| self.tx.ui_width(a.label(), false)).collect();
        let gap = 16;
        let foot_total: i32 = widths.iter().sum::<i32>() + gap * widths.len().saturating_sub(1) as i32;
        let laid = &h.laid.as_ref().unwrap().1;
        let content_w = laid.w.max(foot_total.min(inner_w)).max(110);
        let w = content_w + 2 * PAD_X;
        let rows = footer_rows(&widths, gap, content_w);
        let foot_h = if acts.is_empty() { 0 } else { rows.len() as i32 * FOOT_ROW + 7 };
        let want_h = laid.h + 2 * PAD_Y + foot_h;
        let anchor = self.docs.get(self.active).and_then(|d| d.screen_pos(h.anchor.0)).unwrap_or(h.at);
        let (rect, below) = place(anchor, line_h() as i32, ed, w, want_h, h.side_below);
        h.side_below = Some(below);
        h.rect = rect;
        let view = Rect::new(rect.x + PAD_X, rect.y + PAD_Y, rect.w - 2 * PAD_X, (rect.h - 2 * PAD_Y - foot_h).max(20));
        h.view = view;
        clamp_scroll(h);

        cv.shadow(rect, 10);
        cv.card(rect, 10, chrome.widget_bg, chrome.border);
        let _laid = &h.laid.as_ref().unwrap().1;
        let oy = view.y - h.scroll.round() as i32;
        let clip = Rect::new(rect.x + 1, view.y, rect.w - 2, view.h);
        let (vt, vb) = (h.scroll.round() as i32, h.scroll.round() as i32 + view.h);

        // selection (per character)
        for r in sel_rects(h, &mut self.tx) {
            cv.fill_clip(Rect::new(view.x + r.x, oy + r.y, r.w, r.h), chrome.accent.with_alpha(0x40), clip);
        }
        let laid = &h.laid.as_ref().unwrap().1;
        for d in &laid.decos {
            if d.y + d.h < vt || d.y > vb {
                continue;
            }
            let r = Rect::new(view.x + d.x, oy + d.y, if d.w == 0 { view.w - d.x } else { d.w }, d.h);
            match d.kind {
                DecoKind::Fill(c, rad) => cv.fill_round_clip(r, rad, c, clip),
                DecoKind::Rule(c) => cv.fill_clip(r, c, clip),
            }
        }
        let first = laid.lines.partition_point(|l| l.y + l.h <= vt);
        for l in laid.lines.iter().skip(first) {
            if l.y >= vb {
                break;
            }
            let y = oy + l.y;
            if !l.runs.is_empty() {
                let pieces: Vec<(String, Rgba, u8)> = l.runs.clone();
                self.tx.draw_styled(cv, view.x + l.x0, y, &pieces, clip);
            }
            for s in &l.segs {
                let x = view.x + s.x;
                let hot_link = s.link.is_some_and(|li| self.hv.hot == Some(Hit::Link(laid.urls[li].clone())));
                if s.mono {
                    let ly = y + (l.h - line_h() as i32) / 2;
                    self.tx.draw_line(cv, x, ly, &s.text, s.color, clip);
                } else if let Some(sz) = s.size {
                    self.tx.draw_ui_sized_i(cv, x, y + 2, &s.text, s.color, clip, s.bold, s.italic, sz);
                } else if s.italic {
                    self.tx.draw_ui_italic(cv, x, y, &s.text, s.color, clip, s.bold);
                } else {
                    self.tx.draw_ui(cv, x, y, &s.text, s.color, clip, s.bold);
                }
                if s.link.is_some() {
                    cv.fill_clip(
                        Rect::new(x, y + l.h - 4, s.w, 1),
                        s.color.with_alpha(if hot_link { 0xff } else { 0x70 }),
                        clip,
                    );
                }
                if s.strike {
                    cv.fill_clip(Rect::new(x, y + l.h / 2, s.w, 1), s.color.with_alpha(0xc0), clip);
                }
            }
        }
        // links hit areas
        h.hits.clear();
        for (r, li) in &laid.links {
            let sr = Rect::new(view.x + r.x, oy + r.y, r.w, r.h).intersect(&view);
            if sr.w > 0 && sr.h > 0 {
                h.hits.push((sr, link_target(&laid.urls[*li])));
            }
        }
        // scrollbar
        let track = Rect::new(rect.right() - BAR_W - 2, view.y, BAR_W, view.h);
        h.bar = VBar::new(track, laid.h as f64, view.h as f64, h.scroll);
        if let Some(b) = h.bar {
            let near = (self.mouse.0 >= rect.right() as f64 - 40.0) && rect.contains(self.mouse.0, self.mouse.1);
            b.draw(cv, pal.fg, near, self.hv.drag_bar.is_some());
        }
        // footer
        if foot_h > 0 {
            let fy = rect.bottom() - foot_h;
            cv.fill_clip(Rect::new(rect.x + 1, fy, rect.w - 2, 1), pal.rule, rect);
            let mut k = 0;
            let mut y = fy + 5;
            let fclip = Rect::new(rect.x + 1, fy, rect.w - 2, foot_h - 1);
            for row in &rows {
                let mut x = view.x;
                for _ in row {
                    let a = acts[k];
                    let wd = widths[k];
                    let r = Rect::new(x - 5, y, wd + 10, FOOT_ROW - 2);
                    let hot = self.hv.hot == Some(Hit::Act(a));
                    if hot {
                        cv.fill_round_clip(r, 5, pal.fg.with_alpha(0x1c), fclip);
                    }
                    let col = if hot { pal.link } else { pal.dim };
                    self.tx.draw_ui(cv, x, y + 2, a.label(), col, fclip, false);
                    h.hits.push((r, Hit::Act(a)));
                    x += wd + gap;
                    k += 1;
                }
                y += FOOT_ROW;
            }
        }
        widths.clear();
    }
}
