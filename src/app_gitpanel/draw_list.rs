//! The Changes list: collapsible group headers with hover actions, folders, file rows with a status
//! badge, line counts and hover actions, then the Stashes section (see `stash_ui`).

use super::draw_empty::EmptyBtn;
use super::*;

impl App {
    /// Small icon button of a row (a 22 px square), shown while the row is hovered; its hit is cut
    /// to the part of the row that is visible.
    fn gp_mini_btn(&mut self, cv: &mut Canvas, gp: &mut GitPanel, r: Rect, icon: &str, hit: Hit, vis: Rect) {
        let c = self.theme.chrome;
        let hh = self.hv(r) && self.hv(vis);
        let base = match hit {
            Hit::DiscardAll(_) | Hit::Discard(_) => c.error,
            Hit::Merge(_) => c.warning,
            _ => c.tab_active_fg,
        };
        if hh {
            cv.fill_round_clip(r, 6, base.with_alpha(0x26), vis);
        }
        let tint = if hh || matches!(hit, Hit::Merge(_)) { base } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, vis);
        gp.hits.push((r.intersect(&vis), hit));
    }

    /// Visible part of a row of height `h` at `y` inside `body`.
    fn gp_vis(body: Rect, y: i32, h: i32) -> Rect {
        Rect::new(body.x, y.max(body.y), body.w, ((y + h).min(body.bottom()) - y.max(body.y)).max(0))
    }

    pub(super) fn gp_draw_changes(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let fh = gp.filter_h();
        if fh > 0 {
            self.gp_draw_filter(cv, gp, Rect::new(body.x, body.y, body.w, fh));
        }
        let body = Rect::new(body.x, body.y + fh, body.w, (body.h - fh).max(0));
        let rows = gp.rows();
        if rows.is_empty() && !gp.filter.text.is_empty() {
            let msg = "No matching files";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 36, msg, dim, body, false);
            return;
        }
        if rows.is_empty() {
            let btn = [EmptyBtn { label: "View history", hit: Hit::Tab(Tab::History), primary: false }];
            let copy = ("Nothing to commit", "Working tree clean. Edit a file and it will show up here.");
            self.gp_empty_state(cv, gp, body, ("ui/check", c.git_added), copy, &btn);
            return;
        }
        let total: i32 = rows.iter().map(|r| GitPanel::row_h(*r)).sum::<i32>() + 16;
        gp.max_scroll[0] = (total - body.h).max(0) as f64;
        gp.scroll[0] = gp.scroll[0].clamp(0.0, gp.max_scroll[0]);
        let mut y = body.y - gp.scroll[0] as i32;
        for (k, row) in rows.iter().enumerate() {
            let h = GitPanel::row_h(*row);
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            match *row {
                Row::Header(g) => self.gp_draw_group_row(cv, gp, g, y, body),
                Row::Folder(g, first) => self.gp_draw_folder_row(cv, gp, (g, first), y, body),
                Row::File(i) => self.gp_draw_file_row(cv, gp, i, k, y, body),
                r => self.gp_draw_stash_row(cv, gp, r, k, y, body),
            }
            y += h;
        }
        let (pos, visible) = (gp.scroll[0], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitChanges, body, total as f64, visible, pos, 1.0, false);
    }

    /// The "Filter changes" box above the lists.
    fn gp_draw_filter(&mut self, cv: &mut Canvas, gp: &mut GitPanel, r: Rect) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let f = Rect::new(r.x + COMPOSER_MARGIN, r.y + 4, r.w - 2 * COMPOSER_MARGIN, r.h - 8);
        let on = gp.filter_focus && gp.focused;
        gp.mo.filter_ring.set(on);
        cv.card(f, 7, c.widget_bg, crate::anim::lerp_rgba(c.border, c.accent, gp.mo.filter_ring.v()));
        gp.hits.push((f, Hit::Filter));
        self.tx.icon(cv, "hugeicons/search", f.x + 8, f.y + (f.h - 14) / 2, 14, dim, f);
        let has = !gp.filter.text.is_empty();
        let tx = f.x + 29;
        let right = f.right() - if has { 26 } else { 8 };
        let clip = Rect::new(f.x, f.y, (right - f.x).max(0), f.h);
        let (txt, col) =
            if has { (gp.filter.text.clone(), self.theme.foreground) } else { ("Filter changes".to_string(), dim) };
        if gp.filter.sel_all && has {
            let w = self.tx.ui_width(&txt, false);
            cv.fill_round_clip(Rect::new(tx - 2, f.y + 3, w + 4, f.h - 6), 4, c.accent.with_alpha(0x55), clip);
        }
        self.tx.draw_ui(cv, tx, f.y + (f.h - 18) / 2, &txt, col, clip, false);
        if on {
            let prefix: String = gp.filter.text.chars().take(gp.filter.cursor).collect();
            let cx = tx + self.tx.ui_width(&prefix, false);
            cv.fill_clip(Rect::new(cx, f.y + (f.h - 16) / 2, 1, 16), self.theme.foreground, clip);
        }
        if has {
            let b = Rect::new(f.right() - 24, f.y + (f.h - 20) / 2, 20, 20);
            if self.hv(b) {
                cv.fill_round(b, 5, c.tab_active_fg.with_alpha(0x18));
            }
            self.tx.icon(cv, "ui/close", b.x + 4, b.y + 4, 12, dim, b);
            gp.hits.push((b, Hit::FilterClear));
        }
    }

    /// "Staged" / "Unstaged" / "Untracked": chevron, title, count; the actions of the group show
    /// while the pointer is on the header.
    fn gp_draw_group_row(&mut self, cv: &mut Canvas, gp: &mut GitPanel, g: Group, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let h = SEC_H;
        let vis = Self::gp_vis(body, y, h);
        let n = gp.changes.iter().filter(|ch| ch.group == g).count();
        let hrow = Rect::new(body.x, y, body.w, h);
        let hov = self.hv(hrow) && self.hv(vis);
        if hov {
            cv.fill_round_clip(Rect::new(body.x + 6, y, body.w - 12, h), 8, strong.with_alpha(0x0c), body);
        }
        gp.hits.push((hrow.intersect(&vis), Hit::Toggle(g)));
        let chev = if gp.collapsed.contains(&g) { "ui/chevron_right" } else { "ui/chevron_down" };
        self.tx.icon(cv, chev, body.x + 10, y + (h - 16) / 2, 16, dim, vis);
        let ty = y + (h - 18) / 2;
        self.tx.draw_ui(cv, body.x + 30, ty, g.title(), strong, body, true);
        let tw = self.tx.ui_width(g.title(), true);
        let cnt = n.to_string();
        let nw = self.tx.ui_width(&cnt, true);
        let cw = (nw + 12).max(20);
        let pill = Rect::new(body.x + 30 + tw + 8, y + (h - 16) / 2, cw, 16);
        cv.fill_round_clip(pill, 8, strong.with_alpha(0x1c), body);
        self.tx.draw_ui_sized(cv, pill.x + (cw - nw) / 2, pill.y + 1, &cnt, dim, body, true, 12.0);
        if !hov {
            return;
        }
        let acts: Vec<(&str, Hit)> = match g {
            Group::Staged => vec![("ui/minus", Hit::Section(g))],
            Group::Changes => {
                vec![("ui/plus", Hit::Section(g)), ("ui/undo", Hit::DiscardAll(g)), ("ui/archive", Hit::StashNew)]
            }
            Group::Untracked => vec![("ui/plus", Hit::Section(g)), ("ui/undo", Hit::DiscardAll(g))],
        };
        let mut right = body.right() - 10;
        for (icon, hit) in acts {
            self.gp_mini_btn(cv, gp, Rect::new(right - 22, y + (h - 22) / 2, 22, 22), icon, hit, vis);
            right -= 24;
        }
    }

    /// A folder of a group: chevron, folder icon, path and how many files it holds.
    fn gp_draw_folder_row(&mut self, cv: &mut Canvas, gp: &mut GitPanel, folder: (Group, usize), y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (g, first) = folder;
        let h = FOLDER_H;
        let vis = Self::gp_vis(body, y, h);
        let Some(dir) = gp.changes.get(first).map(|ch| split_name(&ch.path).1.to_string()) else { return };
        let open = !gp.collapsed_dirs.contains(&(g, dir.clone()));
        let r = Rect::new(body.x + 6, y, body.w - 12, h);
        if self.hv(r) && self.hv(vis) {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x0c), body);
        }
        gp.hits.push((r.intersect(&vis), Hit::Folder(g, first)));
        let chev = if open { "ui/chevron_down" } else { "ui/chevron_right" };
        self.tx.icon(cv, chev, r.x + 6, y + (h - 14) / 2, 14, dim, vis);
        let folder_icon = if open { "ui/folder_open" } else { "ui/folder" };
        self.tx.icon(cv, folder_icon, r.x + 24, y + (h - 16) / 2, 16, dim, vis);
        let count = gp.changes.iter().filter(|ch| ch.group == g && split_name(&ch.path).1 == dir).count().to_string();
        let cw = self.tx.ui_width_sized(&count, false, 12.0);
        let name = fit(&mut self.tx, &dir, r.w - 46 - cw - 16, false);
        let ty = y + (h - 18) / 2;
        self.tx.draw_ui(cv, r.x + 46, ty, &name, strong, vis, false);
        self.tx.draw_ui_sized(cv, r.right() - 8 - cw, ty + 2, &count, dim, vis, false, 12.0);
    }

    /// A changed file: icon, name, then on the right either its line counts and status badge, or
    /// (pointer on the row) stage / unstage and discard. A conflicted file always shows its merge button.
    fn gp_draw_file_row(&mut self, cv: &mut Canvas, gp: &mut GitPanel, i: usize, k: usize, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let Some(ch) = gp.changes.get(i).cloned() else { return };
        let h = FILE_H;
        let vis = Self::gp_vis(body, y, h);
        let r = Rect::new(body.x + 6, y, body.w - 12, h);
        let hov = self.hv(r) && self.hv(vis);
        let cur = gp.focused && !gp.msg_focus && gp.cursor == k;
        if gp.selected.contains(&ch.path) {
            cv.fill_round_clip(r, 8, c.accent.with_alpha(if cur || hov { 0x48 } else { 0x30 }), body);
        } else if cur {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), body);
        } else if hov {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x12), body);
        }
        gp.hits.push((r.intersect(&vis), Hit::Row(i)));

        let (name, dir) = split_name(&ch.path);
        let indent = if dir.is_empty() { 0 } else { 14 };
        let ix = r.x + 8 + indent;
        let ty = y + (h - 18) / 2;
        let icon = self.tx.icons.file_icon(name);
        self.tx.icon(cv, &icon, ix, y + (h - 16) / 2, 16, dim, body);

        let conflicted = ch.code == 'U';
        let confirming = gp.confirm == Some(i) && ch.discardable();
        let confirm_w = self.tx.ui_width("Discard?", false) + 16;
        let show = hov || cur;
        // Line counts: "+N -N", dim, left of the badge.
        let stat = gp.stats.as_ref().and_then(|s| s.of(&ch));
        let (stat_w, stat_parts) = match stat {
            Some(s) => {
                let fmt = |v: Option<u32>, sign: char| v.map_or("-".to_string(), |n| format!("{sign}{n}"));
                let (a, d) = (fmt(s.add, '+'), fmt(s.del, '-'));
                let w = self.tx.ui_width_sized(&a, false, 11.0) + 6 + self.tx.ui_width_sized(&d, false, 11.0);
                (w + 8, Some((a, d)))
            }
            None => (0, None),
        };
        // Room kept on the right: the widest of the two states, so the name does not jump.
        let acts_w = (if conflicted { 0 } else { 24 })
            + if !ch.discardable() {
                0
            } else if confirming {
                confirm_w + 2
            } else {
                24
            }
            + 4;
        let reserve = (if conflicted { 24 } else { 0 }) + acts_w.max(stat_w + 24);
        let avail = r.w - (8 + indent + 22 + 8) - reserve;
        let nm = fit(&mut self.tx, name, avail, false);
        self.tx.draw_ui(cv, ix + 22, ty, &nm, if ch.code == 'D' { dim } else { strong }, vis, false);

        let mut right = r.right() - 4;
        let by = y + (h - 22) / 2;
        if conflicted {
            self.gp_mini_btn(cv, gp, Rect::new(right - 22, by, 22, 22), "ui/git_merge_conflict", Hit::Merge(i), vis);
            right -= 24;
        }
        if show || confirming {
            if !conflicted {
                let icon = if ch.check_state() == CheckState::On { "ui/minus" } else { "ui/plus" };
                self.gp_mini_btn(cv, gp, Rect::new(right - 22, by, 22, 22), icon, Hit::Check(i), vis);
                right -= 24;
            }
            if confirming {
                let br = Rect::new(right - confirm_w, y + (h - 24) / 2, confirm_w, 24);
                let bh = self.hv(br);
                cv.fill_round_clip(br, 8, c.error.with_alpha(if bh { 0x55 } else { 0x33 }), vis);
                self.tx.draw_ui(cv, br.x + 8, br.y + 3, "Discard?", c.error, br.intersect(&vis), false);
                gp.hits.push((br.intersect(&vis), Hit::ConfirmDiscard(i)));
            } else if ch.discardable() {
                self.gp_mini_btn(cv, gp, Rect::new(right - 22, by, 22, 22), "ui/undo", Hit::Discard(i), vis);
            }
        } else {
            let letter = if ch.code == '?' { 'U' } else { ch.code };
            let text = letter.to_string();
            let lw = self.tx.ui_width(&text, true);
            let tint = code_color(&c, ch.code);
            let chip = Rect::new(right - 20, y + (h - 18) / 2, 18, 18);
            cv.fill_round_clip(chip, 6, tint.with_alpha(0x26), vis);
            self.tx.draw_ui(cv, chip.x + (18 - lw) / 2, chip.y, &text, tint, chip.intersect(&vis), true);
            if let Some((a, d)) = stat_parts {
                let aw = self.tx.ui_width_sized(&a, false, 11.0);
                let sx = chip.x - 4 - stat_w + 8;
                self.tx.draw_ui_sized(cv, sx, ty + 2, &a, c.git_added.with_alpha(0x99), vis, false, 11.0);
                self.tx.draw_ui_sized(cv, sx + aw + 6, ty + 2, &d, c.error.with_alpha(0x99), vis, false, 11.0);
            }
        }
    }
}
