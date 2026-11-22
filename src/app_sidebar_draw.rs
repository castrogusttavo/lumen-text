//! Drawing of the explorer sidebar: header (project name, filter field, refresh), the tree rows
//! (glide highlight, indent guides, chevron cross-fade, git decoration), the outline and the
//! empty states. All motion values come from `app_sidebar_fx::TreeFx`; colors from the theme.

use crate::{
    anim::{fade, lerp_rgba},
    app::{App, Focus, ROW_H, SidebarMode, TAB_H},
    app_sidebar_fx::{EmptyAct, Hits},
    ds,
    theme::Rgba,
    ui::{Canvas, Rect},
};
use std::path::Path;

/// Buttons of the header are square, `BTN` px, the first one sits left of the refresh button.
const BTN: i32 = 26;
/// Height of the filter field.
const FIELD_H: i32 = 28;
/// How much of a row's git color is mixed into its name.
const TINT: f32 = 0.5;
/// Opacity of rows that do not match the filter, and of folders that only hold a match.
const FILTER_OFF: f32 = 0.35;
const FILTER_PARENT: f32 = 0.7;
/// Opacity of cut / ignored rows (the old 0x60 alpha).
const DIM: f32 = 0.376;

impl App {
    /// The whole sidebar card: background, header, then the tree or the outline.
    pub(crate) fn draw_sidebar_ui(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        self.layout.sidebar_tabs.clear();
        self.sidebar_bar = None;
        self.tree.fx.hits = Hits::default();
        self.draw_sidebar_header(cv);
        if self.sidebar_mode == SidebarMode::Outline {
            self.draw_outline(cv);
        } else {
            self.draw_tree_rows(cv);
        }
        self.draw_sidebar_bar(cv);
    }

    fn header_btn(&self, slot: i32) -> Rect {
        let r = self.layout.sidebar;
        Rect::new(r.right() - 1 - 10 - BTN - slot * (BTN + 2), r.y + (TAB_H - BTN) / 2 + 2, BTN, BTN)
    }

    fn draw_icon_btn(&mut self, cv: &mut Canvas, b: Rect, icon: &str, hot: f32, clip: Rect) {
        let c = self.theme.chrome;
        if hot > 0.01 {
            cv.fill_round(b, ds::R_CTRL, fade(c.list_hover_bg, hot));
        }
        let col = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
        self.tx.icon(cv, icon, b.x + (b.w - ds::ICON_S) / 2, b.y + (b.h - ds::ICON_S) / 2, ds::ICON_S, col, clip);
    }

    fn draw_sidebar_header(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let ty = r.y + (TAB_H - 18) / 2 + 2;
        self.draw_explorer_refresh(cv);
        if self.sidebar_mode == SidebarMode::Outline {
            let n = self.docs.get(self.active).map_or(0, |d| d.symbols.len());
            self.tx.icon(cv, "ui/list_tree", r.x + 16, ty + 2, ds::ICON_S, c.tab_inactive_fg, r);
            let x = r.x + 16 + ds::ICON_S + 8;
            self.tx.draw_ui(cv, x, ty, "Outline", c.tab_active_fg, r, true);
            if n > 0 {
                let w = self.tx.ui_width("Outline", true);
                self.tx.draw_ui_sized(cv, x + w + 8, ty + 2, &n.to_string(), c.tab_inactive_fg, r, false, 11.0);
            }
            return;
        }
        if self.tree.fx.filter_open {
            return self.draw_filter_field(cv);
        }
        let search = self.header_btn(1);
        self.tree.fx.hits.search = Some(search);
        let name =
            self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".to_string());
        self.tx.icon(cv, "ui/folder_open", r.x + 16, ty + 2, ds::ICON_S, c.tab_inactive_fg, r);
        let x = r.x + 16 + ds::ICON_S + 8;
        let title = crate::ui::fit_ellipsis(&mut self.tx, &name, search.x - 6 - x, true);
        self.tx.draw_ui(cv, x, ty, &title, c.tab_active_fg, r, true);
        let hot = self.tree.fx.search_hot.v();
        self.draw_icon_btn(cv, search, "hugeicons/search", hot, r);
    }

    /// The filter input that replaces the project title while it is open.
    fn draw_filter_field(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let refresh = self.header_btn(0);
        let f =
            Rect::new(r.x + ds::S2, r.y + (TAB_H - FIELD_H) / 2 + 2, (refresh.x - 4 - (r.x + ds::S2)).max(40), FIELD_H);
        let ring = self.tree.fx.ring.v();
        cv.fill_round(f, ds::R_CTRL, c.input_bg);
        cv.ring(f, ds::R_CTRL, lerp_rgba(c.border, c.accent, ring));
        let icon_col = lerp_rgba(c.tab_inactive_fg, c.accent_text, ring);
        self.tx.icon(cv, "hugeicons/search", f.x + 8, f.y + (f.h - ds::ICON_S) / 2, ds::ICON_S, icon_col, f);
        let clear = Rect::new(f.right() - 4 - 20, f.y + 4, 20, 20);
        let text = self.tree.fx.filter.text.clone();
        let hits = self.tree.fx.flags.iter().filter(|&&k| k == 2).count();
        let (label, label_col) = if text.is_empty() {
            (String::new(), c.tab_inactive_fg)
        } else if hits == 0 {
            ("No match".to_string(), c.warning.readable_on(&[c.input_bg], 4.5))
        } else {
            (hits.to_string(), c.tab_inactive_fg)
        };
        let lw = if label.is_empty() { 0 } else { self.tx.ui_width_sized(&label, false, 11.0) };
        let label_x = clear.x - 4 - lw;
        if !label.is_empty() {
            self.tx.draw_ui_sized(cv, label_x, f.y + (f.h - 15) / 2, &label, label_col, f, false, 11.0);
        }
        let hot = self.tree.fx.clear_hot.v();
        if hot > 0.01 {
            cv.fill_round(clear, ds::R_CTRL, fade(c.list_hover_bg, hot));
        }
        let x_col = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
        self.tx.icon(cv, "hugeicons/close", clear.x + 4, clear.y + 4, 12, x_col, f);
        // Text, selection and caret (the text scrolls left when it outgrows the field).
        let text_x = f.x + 8 + ds::ICON_S + 6;
        let room = (label_x - 6 - text_x).max(0);
        let clip = Rect::new(text_x, f.y, room, f.h);
        let ty = f.y + (f.h - 18) / 2;
        let cursor = self.tree.fx.filter.cursor.min(text.chars().count());
        let pre: String = text.chars().take(cursor).collect();
        let caret_w = self.tx.ui_width(&pre, false);
        let off = (caret_w - room + 2).max(0);
        if text.is_empty() {
            self.tx.draw_ui(cv, text_x + 2, ty, "Filter files", c.tab_inactive_fg, clip, false);
        } else {
            if let Some((a, b)) = self.tree.fx.filter.range() {
                let xa = self.tx.ui_width(&text.chars().take(a).collect::<String>(), false);
                let xb = self.tx.ui_width(&text.chars().take(b).collect::<String>(), false);
                cv.fill_clip(Rect::new(text_x + xa - off, f.y + 5, xb - xa, f.h - 10), c.accent.with_alpha(0x50), clip);
            }
            self.tx.draw_ui(cv, text_x - off, ty, &text, c.sidebar_fg, clip, false);
        }
        if ring > 0.01 {
            cv.fill_clip(Rect::new(text_x + caret_w - off, f.y + 7, 1, f.h - 14), fade(c.accent_text, ring), clip);
        }
        self.tree.fx.hits.field = Some(f);
        self.tree.fx.hits.clear = Some(clear);
    }

    fn draw_tree_rows(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        if self.tree.rows.is_empty() {
            return self.draw_tree_empty(cv, body);
        }
        let scroll = self.tree.scroll as i32;
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -(scroll % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let focused = self.focus == Focus::Sidebar;
        let single = self.tree.selected.is_empty();
        let cursor_row = self.tree.sel;
        let active_path = self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf);
        let rows = ((body.h / ROW_H) + 2) as usize;
        let row_rect = |y: i32| Rect::new(r.x + ds::S2, y + 1, r.w - 17, ROW_H - 2);
        // Highlights that glide between rows: pointer hover, then the keyboard cursor with its bar.
        let (ha, hy, ca, cy) = {
            let fx = &self.tree.fx;
            (fx.hover_a.v(), fx.hover_y.value, fx.cursor_a.v(), fx.cursor_y.value)
        };
        if ha > 0.01 {
            let rr = row_rect(top - scroll + hy.round() as i32);
            cv.fill_round_clip(rr, ds::R_ROW, fade(c.list_hover_bg.with_alpha(0xa0), ha), body);
        }
        if ca > 0.01 {
            let rr = row_rect(top - scroll + cy.round() as i32);
            cv.fill_round_clip(rr, ds::R_ROW, fade(c.list_active_bg, ca), body);
            cv.fill_round_clip(Rect::new(rr.x + 1, rr.y + 8, 3, rr.h - 16), 2, fade(c.accent, ca), body);
        }
        let guides = self.tree.fx.guides.v();
        let filtering = !self.tree.fx.filter.text.is_empty();
        let query = self.tree.fx.filter.text.to_lowercase();
        let bg = c.sidebar_bg;
        let soften = |col: Rgba, k: f32| if k >= 0.999 { col } else { fade(col, k).over(bg) };
        for (k, row) in self.tree.rows.iter().enumerate().skip(first).take(rows) {
            let y = top + off + ((k - first) as i32) * ROW_H;
            let rr = row_rect(y);
            let is_active = active_path.as_deref() == Some(row.path.as_path());
            let on_cursor = single && focused && k == cursor_row && ca > 0.01;
            if !single && focused && self.tree.row_selected(k) {
                cv.fill_round_clip(rr, ds::R_ROW, c.list_active_bg, body);
            } else if is_active && !on_cursor {
                cv.fill_round_clip(rr, ds::R_ROW, c.list_hover_bg, body);
            }
            let indent = 18 + row.depth as i32 * 14;
            // Indent guides: one hairline per ancestor level, fading in while the pointer is over the tree.
            if guides > 0.01 && row.depth > 0 {
                let line = fade(c.border, guides * 0.7);
                for lvl in 0..row.depth as i32 {
                    cv.fill_clip(Rect::new(r.x + hx + 18 + lvl * 14 + 3, y, 1, ROW_H), line, body);
                }
            }
            let cut = self.file_clip.is_cut(&row.path);
            let mut k_alpha = if cut || row.ignored { DIM } else { 1.0 };
            if filtering {
                k_alpha *= match self.tree.fx.flags.get(k) {
                    Some(2) => 1.0,
                    Some(1) => FILTER_PARENT,
                    _ => FILTER_OFF,
                };
            }
            let icon_color = soften(if is_active { c.tab_active_fg } else { c.tab_inactive_fg }, k_alpha);
            let iy = y + (ROW_H - 16) / 2;
            if row.is_dir {
                // Chevron cross-fade (closed -> open) and the folder icon swapping at the midpoint.
                let a = self
                    .tree
                    .fx
                    .chev
                    .get(&row.path)
                    .map_or(f32::from(u8::from(row.expanded)), |a| a.value.clamp(0.0, 1.0));
                let (cx, cy) = (r.x + hx + indent - 4, y + (ROW_H - 14) / 2);
                if a < 0.99 {
                    self.tx.icon(cv, "ui/chevron_right", cx, cy, 14, fade(icon_color, 1.0 - a), body);
                }
                if a > 0.01 {
                    self.tx.icon(cv, "ui/chevron_down", cx, cy, 14, fade(icon_color, a), body);
                }
                let folder = if a > 0.5 { "file/folder_open" } else { "file/folder" };
                self.tx.icon(cv, folder, r.x + hx + indent + 14, iy, 16, icon_color, body);
            } else {
                let name = self.tx.icons.file_icon(&row.name);
                self.tx.icon(cv, &name, r.x + hx + indent + 14, iy, 16, icon_color, body);
            }
            // Git decoration: a changed file tints its name and shows a dot; a collapsed folder
            // shows the dot of the strongest change inside it.
            let status = if row.is_dir {
                if row.expanded { None } else { self.tree.fx.dir_badge.get(&row.path).copied() }
            } else {
                self.git_status.get(&row.path).copied()
            };
            let mut fg = if is_active { c.tab_active_fg } else { c.sidebar_fg };
            if !row.is_dir
                && let Some(b) = status
            {
                let tint = match b {
                    'D' => c.git_deleted,
                    'M' | 'R' => c.git_modified,
                    _ => c.git_added,
                };
                fg = fg.mix(tint, TINT);
            }
            let fg = soften(fg, k_alpha);
            // A name that does not fit before the git dot ends in "…"; scrolling right reveals it.
            let text_x = r.x + hx + indent + 38;
            // Line coverage of the file, when a report is loaded, just before the git dot.
            let cov = if row.is_dir { None } else { self.coverage_pct_of(&row.path) };
            let cov_label = cov.map(|p| format!("{p:.0}%"));
            let cov_w = cov_label.as_deref().map_or(0, |l| self.tx.ui_width_sized(l, false, 11.0) + 6);
            let room = crate::app_sidebar::name_room(r, text_x, false) - cov_w;
            let name = crate::ui::fit_ellipsis(&mut self.tx, &row.name, room, false);
            if filtering
                && self.tree.fx.flags.get(k) == Some(&2)
                && name.is_ascii()
                && let Some(i) = name.to_ascii_lowercase().find(&query)
            {
                let pre = self.tx.ui_width(&name[..i], false);
                let mw = self.tx.ui_width(&name[i..i + query.len()], false);
                cv.fill_round_clip(
                    Rect::new(text_x + pre - 2, y + (ROW_H - 18) / 2 + 1, mw + 4, 16),
                    3,
                    c.accent.with_alpha(0x40),
                    body,
                );
            }
            self.tx.draw_ui(cv, text_x, y + (ROW_H - 18) / 2, &name, fg, body, false);
            if let (Some(p), Some(l)) = (cov, cov_label.as_deref()) {
                let col = self.coverage_color(p).with_alpha(0xc0).over(c.sidebar_bg);
                self.tx.draw_ui_sized(cv, r.right() - 30 - cov_w, y + (ROW_H - 15) / 2, l, col, body, false, 11.0);
            }
            if let Some(b) = status {
                // A quiet dot instead of a colored letter: modified / new / deleted.
                let dot = match b {
                    'D' => c.git_deleted,
                    'M' | 'R' => c.git_modified,
                    _ => c.git_added.with_alpha(0xa0),
                };
                cv.fill_round_clip(
                    Rect::new(r.right() - 24, y + ROW_H / 2 - 3, 6, 6),
                    3,
                    soften(dot, k_alpha.max(0.5)),
                    body,
                );
            }
        }
    }

    /// Friendly empty folder: icon, one line, one hint and the two actions that fill it.
    fn draw_tree_empty(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let t = self.tree.fx.empty_t.value.clamp(0.0, 1.0);
        let ghost = |col: Rgba| fade(col, t).over(c.sidebar_bg);
        let rise = ((1.0 - t) * 8.0).round() as i32;
        let cx = body.x + body.w / 2;
        let mut y = body.y + ((body.h - 170) / 2).clamp(16, 56) + rise;
        self.tx.icon(cv, "ui/folder_open", cx - 14, y, 28, ghost(c.tab_inactive_fg), body);
        y += 28 + 12;
        let title = "This folder is empty";
        let w = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, cx - w / 2, y, title, ghost(c.tab_active_fg), body, true);
        y += 24;
        let hint = crate::ui::fit_ellipsis(&mut self.tx, "Create a file or a folder to start", body.w - 32, false);
        let w = self.tx.ui_width(&hint, false);
        self.tx.draw_ui(cv, cx - w / 2, y, &hint, ghost(c.tab_inactive_fg), body, false);
        y += 20 + 14;
        let bw = (body.w - 48).clamp(80, 180);
        for (label, key, act, primary) in
            [("New file", "A", EmptyAct::NewFile, true), ("New folder", "Shift+A", EmptyAct::NewFolder, false)]
        {
            let b = Rect::new(cx - bw / 2, y, bw, ds::HIT_MIN + 4);
            self.draw_empty_btn(cv, b, label, key, primary, t, body);
            if t > 0.5 {
                self.tree.fx.hits.empty.push((b, act));
            }
            y += b.h + 8;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_empty_btn(&mut self, cv: &mut Canvas, b: Rect, label: &str, key: &str, primary: bool, t: f32, clip: Rect) {
        let c = self.theme.chrome;
        let hot = self.overlay.is_none() && b.contains(self.mouse.0, self.mouse.1);
        let (fill, fg, border) = if primary {
            let f = if hot { c.accent.mix(Rgba(255, 255, 255, 255), 0.12) } else { c.accent };
            (f, c.on_accent, f)
        } else {
            (if hot { c.list_hover_bg.over(c.widget_bg) } else { c.widget_bg }, c.tab_active_fg, c.border)
        };
        let g = |col: Rgba| fade(col, t).over(c.sidebar_bg);
        cv.fill_round_clip(b, ds::R_CTRL, g(fill), clip);
        cv.ring(b, ds::R_CTRL, g(border));
        self.tx.draw_ui(cv, b.x + 12, b.y + (b.h - 18) / 2, label, g(fg), clip, false);
        let kw = self.tx.ui_width_sized(key, false, 11.0);
        let chip = Rect::new(b.right() - 8 - kw - 10, b.y + (b.h - 18) / 2, kw + 10, 18);
        cv.fill_round_clip(chip, 4, g(fg).with_alpha(0x28), clip);
        self.tx.draw_ui_sized(cv, chip.x + 5, chip.y + 1, key, g(fg), clip, false, 11.0);
    }

    fn draw_outline(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        let Some(d) = self.docs.get(self.active) else { return };
        if d.symbols.is_empty() {
            let dim = c.tab_inactive_fg;
            let lines = [
                ("Functions, classes and types of the open file".to_string(), dim),
                ("show up here once its language server is ready".to_string(), dim),
            ];
            return self.b_empty_state(cv, body, "ui/list_tree", dim, "No outline", &lines);
        }
        let (cur_line, _) = d.buf.line_col(d.ed.head());
        let chain: Vec<usize> = d.symbol_chain(cur_line).iter().map(|s| s.sel_l).collect();
        let scroll = self.tree.scroll as i32;
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -(scroll % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let rows = (body.h / ROW_H + 2) as usize;
        let syms: Vec<crate::doc::Sym> = d.symbols.iter().skip(first).take(rows).cloned().collect();
        let row_rect = |y: i32| Rect::new(r.x + ds::S2, y + 1, r.w - 17, ROW_H - 2);
        let (ha, hy) = (self.tree.fx.hover_a.v(), self.tree.fx.hover_y.value);
        if ha > 0.01 {
            let rr = row_rect(top - scroll + hy.round() as i32);
            cv.fill_round_clip(rr, ds::R_ROW, fade(c.list_hover_bg.with_alpha(0xa0), ha), body);
        }
        for (k, s) in syms.iter().enumerate() {
            let y = top + off + k as i32 * ROW_H;
            let rr = row_rect(y);
            if chain.contains(&s.sel_l) {
                cv.fill_round_clip(rr, ds::R_ROW, c.list_hover_bg, body);
                if chain.last() == Some(&s.sel_l) {
                    cv.fill_round_clip(Rect::new(rr.x + 1, rr.y + 8, 3, rr.h - 16), 2, c.accent, body);
                }
            }
            let x = r.x + hx + 18 + s.depth as i32 * 14;
            let letter = match s.kind {
                5 => "C",
                6 => "m",
                7 => "p",
                8 => "f",
                9 => "c",
                10 => "E",
                11 => "I",
                12 => "ƒ",
                13 => "v",
                14 => "k",
                2 => "M",
                22 => "e",
                23 => "S",
                26 => "T",
                _ => "·",
            };
            // The kind sits in a small tinted chip, so the column of names stays aligned.
            let chip = Rect::new(x - 2, y + (ROW_H - 18) / 2, 16, 18);
            cv.fill_round_clip(chip, ds::R_CTRL - 2, c.accent.with_alpha(0x24), body);
            let lw = self.tx.ui_width(letter, true);
            self.tx.draw_ui(cv, chip.x + (chip.w - lw) / 2, chip.y, letter, c.accent_text, body, true);
            let room = crate::app_sidebar::name_room(r, x + 18, true);
            let name = crate::ui::fit_ellipsis(&mut self.tx, &s.name, room, false);
            self.tx.draw_ui(cv, x + 18, y + (ROW_H - 18) / 2, &name, c.sidebar_fg, body, false);
        }
    }
}
