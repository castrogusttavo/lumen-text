//! The detail pane: its top bar, the status line of a running change, and the items of a pull
//! request / issue / run measured and painted in a scrolling column.

use super::*;
use crate::scrollreg::Reg;
use items::{Btn, BtnStyle, It, Row, review_word};
use md::{Kind, MdLine};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

const PAD: i32 = 24;
const COL_MAX: i32 = 840;
pub(super) const BAR_H: i32 = 44;
const NOTE_H: i32 = 26;
const CODE_H: i32 = 18;
const MSG_PAD_X: i32 = 10;
const MSG_PAD_Y: i32 = 8;
/// Diff lines drawn per file (the rest is a "… more lines" row).
const DIFF_MAX: usize = 400;

impl App {
    /// The detail pane (right of the list, or the whole page in a narrow window).
    pub(super) fn gh_draw_detail(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect, two: bool) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        if matches!(g.detail, Detail::None) {
            self.gh_draw_empty_detail(cv, g, r);
            return;
        }
        // Top bar: back (narrow), what it is, then open / copy / refresh.
        let bar = Rect::new(r.x, r.y, r.w, BAR_H);
        cv.fill(Rect::new(r.x, bar.bottom() - 1, r.w, 1), c.border);
        let mut x = r.x + 10;
        if !two {
            let b = Rect::new(x, r.y + 8, 28, 28);
            self.gh_icon_btn(cv, g, b, "ui/arrow_left", GhHit::Back, false);
            x += 34;
        }
        let (icon, label) = match &g.detail {
            Detail::Pr(v) => ("ui/pull_request", format!("Pull request #{}", v.number)),
            Detail::Issue(v) => ("ui/info", format!("Issue #{}", v.number)),
            Detail::Run(v) => ("ui/bolt_filled", format!("Run #{}", v.id)),
            Detail::None => ("ui/github", String::new()),
        };
        self.tx.icon(cv, icon, x + 4, r.y + 14, 16, dim, bar);
        self.tx.draw_ui(cv, x + 28, r.y + Self::gh_ty(BAR_H), &label, strong, bar, true);
        let mut right = r.right() - 10;
        for (ic, hit) in
            [("ui/rotate_cw", GhHit::Refresh), ("ui/link", GhHit::CopyLink), ("ui/arrow_up_right", GhHit::OpenWeb)]
        {
            let b = Rect::new(right - 28, r.y + 8, 28, 28);
            self.gh_icon_btn(cv, g, b, ic, hit, false);
            right -= 30;
        }
        let mut top = bar.bottom();
        // What is running, or how the last change went.
        let note = match (&g.busy, &g.note) {
            (Some(b), _) => Some((b.clone(), Tone::Accent)),
            (None, Some((t, ok, at))) if at.elapsed() < Duration::from_secs(10) => {
                Some((t.clone(), if *ok { Tone::Success } else { Tone::Error }))
            }
            _ => None,
        };
        if let Some((t, tone)) = note {
            let nr = Rect::new(r.x, top, r.w, NOTE_H);
            let col = self.gh_color(tone);
            cv.fill(nr, col.with_alpha(0x14));
            let t = fit(&mut self.tx, &t, r.w - 28, false);
            self.tx.draw_ui(cv, r.x + 14, nr.y + Self::gh_ty(NOTE_H), &t, col, nr, false);
            top += NOTE_H;
        }
        let body = Rect::new(r.x, top, r.w, (r.bottom() - top).max(0));
        if matches!(&g.detail, Detail::Run(v) if v.job.is_some()) {
            self.gh_draw_log(cv, g, body);
            return;
        }
        self.gh_draw_items(cv, g, body);
    }

    fn gh_draw_empty_detail(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect) {
        let c = self.theme.chrome;
        let cx = r.x + r.w / 2;
        let y = r.y + (r.h / 3).max(40);
        let badge = Rect::new(cx - 24, y, 48, 48);
        cv.fill_round(badge, 24, c.tab_active_fg.with_alpha(0x12));
        self.tx.icon(cv, g.section.icon(), badge.x + 12, badge.y + 12, 24, c.tab_inactive_fg, r);
        let msg = match g.section {
            Section::Inbox => {
                let n = g.inbox.items.as_ref().map_or(0, |v| v.iter().filter(|n| n.unread).count());
                if n == 0 { "You are all caught up".to_string() } else { model::plural(n, "unread notification") }
            }
            s => format!("Select a {} to read it", s.noun().trim_end_matches('s')),
        };
        let w = self.tx.ui_width(&msg, true);
        self.tx.draw_ui(cv, cx - w / 2, y + 62, &msg, c.tab_active_fg, r, true);
        let hint = match g.section {
            Section::Inbox => "Click a notification to open it; it is marked as read.",
            Section::Actions => "Jobs, steps and logs show here.",
            _ => "The description, comments, files and checks show here.",
        };
        let hw = self.tx.ui_width(hint, false);
        self.tx.draw_ui(cv, cx - hw / 2, y + 86, hint, c.tab_inactive_fg, r, false);
        if g.section == Section::Inbox && g.inbox.items.as_ref().is_some_and(|v| v.iter().any(|n| n.unread)) {
            let b = Btn {
                label: "Mark all read".into(),
                icon: "ui/check",
                hit: GhHit::Ask(Act::MarkAllRead),
                style: BtnStyle::Plain,
            };
            let bw = 12 + 16 + 6 + self.tx.ui_width(&b.label, false) + 12;
            self.gh_button(cv, g, cx - bw / 2, y + 116, &b, r);
        }
        if let Some((t, ok, _)) = g.note.clone().filter(|(_, _, at)| at.elapsed() < Duration::from_secs(10)) {
            let col = if ok { c.git_added } else { c.error };
            let tw = self.tx.ui_width(&t, false);
            self.tx.draw_ui(cv, cx - tw / 2, y + 160, &t, col, r, false);
        }
    }

    // ───────────── measuring ─────────────

    /// Wrapped lines of `text` at `w` px: tag 0 = Markdown, 1 = the title, 2 = plain.
    pub(super) fn gh_wrapped(&mut self, g: &mut GhPanel, text: &str, w: i32, tag: u8) -> Arc<Vec<MdLine>> {
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        tag.hash(&mut h);
        let key = (h.finish(), w);
        if let Some(v) = g.wrap_cache.get(&key) {
            return v.clone();
        }
        if g.wrap_cache.len() > 800 {
            g.wrap_cache.clear();
        }
        let tx = &mut self.tx;
        let plain = |tx: &mut crate::ui::TextCtx, s: &str, w: i32, kind: Kind, bold: bool| -> Vec<MdLine> {
            md::wrap(s, w, &mut |x| tx.ui_width(x, bold)).into_iter().map(|text| MdLine { kind, text }).collect()
        };
        let lines: Vec<MdLine> = match tag {
            0 => {
                let mut out = Vec::new();
                for l in md::lines(text) {
                    match l.kind {
                        Kind::Blank | Kind::Code => out.push(l),
                        Kind::H1 | Kind::H2 => out.extend(plain(tx, &l.text, w, l.kind, true)),
                        Kind::Bullet => {
                            for (i, t) in plain(tx, &l.text, w - 18, Kind::Bullet, false).into_iter().enumerate() {
                                out.push(MdLine {
                                    kind: Kind::Bullet,
                                    text: format!("{}{}", if i == 0 { "• " } else { "  " }, t.text),
                                });
                            }
                        }
                        Kind::Quote => out.extend(plain(tx, &l.text, w - 14, Kind::Quote, false)),
                        Kind::Text => out.extend(plain(tx, &l.text, w, Kind::Text, false)),
                    }
                }
                out
            }
            1 => {
                let size = crate::ui::ui_size() + 4.0;
                md::wrap(text, w, &mut |x| tx.ui_width_sized(x, true, size))
                    .into_iter()
                    .map(|text| MdLine { kind: Kind::H1, text })
                    .collect()
            }
            _ => plain(tx, text, w, Kind::Text, false),
        };
        let v = Arc::new(lines);
        g.wrap_cache.insert(key, v.clone());
        v
    }

    fn md_h(lines: &[MdLine]) -> i32 {
        lines
            .iter()
            .map(|l| match l.kind {
                Kind::Blank => 8,
                Kind::Code => CODE_H,
                Kind::H1 => 28,
                Kind::H2 => 24,
                _ => 20,
            })
            .sum::<i32>()
            + 4
    }

    fn title_lh() -> i32 {
        (crate::ui::ui_line() + 6.0).round() as i32
    }

    fn gh_btn_w(&mut self, b: &Btn) -> i32 {
        12 + 16 + 6 + self.tx.ui_width(&b.label, false) + 12
    }

    /// Rows of buttons that fit `w` (button count per row).
    fn gh_btn_rows(&mut self, bs: &[Btn], w: i32) -> Vec<usize> {
        let widths: Vec<i32> = bs.iter().map(|b| self.gh_btn_w(b)).collect();
        crate::app_gitpanel::button_rows(&widths, w, 6)
    }

    fn gh_chips_w(&mut self, l: &ghub::Label) -> i32 {
        8 + 8 + 6 + self.tx.ui_width(&l.name, false) + 10
    }

    fn gh_chip_rows(&mut self, labels: &[ghub::Label], w: i32) -> Vec<usize> {
        let widths: Vec<i32> = labels.iter().map(|l| self.gh_chips_w(l)).collect();
        crate::app_gitpanel::button_rows(&widths, w, 6)
    }

    fn gh_row_h(r: &Row) -> i32 {
        if r.sub.is_empty() { 34 } else { 46 }
    }

    fn gh_comment_h(&mut self, g: &mut GhPanel, c: &ghub::Comment, cw: i32) -> i32 {
        if c.body.trim().is_empty() {
            return 38;
        }
        let lines = self.gh_wrapped(g, &c.body, cw - 28, 0);
        38 + 8 + Self::md_h(&lines) + 8
    }

    /// Keep the composer laid out at the width of the column, and size its box.
    fn gh_composer_layout(&mut self, g: &mut GhPanel, cw: i32) -> i32 {
        let lh = (crate::ui::ui_line().ceil() as i32).max(18);
        let tx = &mut self.tx;
        g.composer.layout((cw - 2 * MSG_PAD_X).max(1) as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
        let lines = (g.composer.line_count() as i32).clamp(3, 10);
        g.composer.set_view(lh as f32, (lines * lh) as f32);
        lines * lh + 2 * MSG_PAD_Y
    }

    fn gh_item_h(&mut self, g: &mut GhPanel, it: &It, cw: i32) -> i32 {
        match it {
            It::Title(t) => self.gh_wrapped(g, t, cw, 1).len() as i32 * Self::title_lh() + 6,
            It::Badges(_) => 26,
            It::Dim(t) => self.gh_wrapped(g, t, cw, 2).len() as i32 * 20,
            It::Note(t, _) => self.gh_wrapped(g, t, cw, 2).len() as i32 * 20 + 6,
            It::Space(n) => *n,
            It::Buttons(b) => {
                let rows = self.gh_btn_rows(b, cw).len() as i32;
                rows * 28 + (rows - 1).max(0) * 6
            }
            It::Confirm(c) => {
                let lines = self.gh_wrapped(g, &c.text, cw - 28, 2).len() as i32;
                10 + lines * 20 + 8 + 28 + 10
            }
            It::Tabs(_) => 34,
            It::Heading(..) => 28,
            It::Md(b) => {
                let lines = self.gh_wrapped(g, b, cw, 0);
                Self::md_h(&lines)
            }
            It::Chips(l) => {
                let rows = self.gh_chip_rows(l, cw).len() as i32;
                rows * 24 + (rows - 1).max(0) * 6
            }
            It::Kv(..) => 24,
            It::Comment(c) => self.gh_comment_h(g, c, cw),
            It::Row(r) => Self::gh_row_h(r),
            It::Diff(i) => {
                let n = g.diff_lines(*i).map_or(1, |(n, _)| n.min(DIFF_MAX) + usize::from(n > DIFF_MAX));
                n as i32 * crate::ui::line_h().ceil() as i32 + 12
            }
            It::Composer(b) => {
                let bh = self.gh_composer_layout(g, cw);
                let rows = self.gh_btn_rows(b, cw).len() as i32;
                28 + bh + 10 + rows * 28 + (rows - 1).max(0) * 6 + 6
            }
        }
    }

    // ───────────── painting ─────────────

    pub(super) fn gh_draw_items(&mut self, cv: &mut Canvas, g: &mut GhPanel, body: Rect) {
        let its = g.detail_items(now_secs());
        let cw = (body.w - 2 * PAD).clamp(120, COL_MAX);
        let col_x = body.x + (body.w - cw) / 2;
        let mut hs = Vec::with_capacity(its.len());
        for it in &its {
            hs.push(self.gh_item_h(g, it, cw));
        }
        let total: i32 = hs.iter().sum::<i32>() + 2 * PAD;
        g.scroll[1] = g.scroll[1].clamp(0.0, (total - body.h).max(0) as f64);
        let mut y = body.y + PAD - g.scroll[1] as i32;
        g.composer_box = Rect::default();
        for (it, &h) in its.iter().zip(&hs) {
            if y + h >= body.y && y <= body.bottom() {
                self.gh_draw_item(cv, g, it, Rect::new(col_x, y, cw, h), body);
            }
            y += h;
        }
        let (pos, visible) = (g.scroll[1], body.h as f64);
        self.region_bar(cv, Reg::GhDetail, body, total as f64, visible, pos, 1.0, false);
    }

    fn gh_draw_item(&mut self, cv: &mut Canvas, g: &mut GhPanel, it: &It, r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        match it {
            It::Space(_) => {}
            It::Title(t) => {
                let lines = self.gh_wrapped(g, t, r.w, 1);
                let size = crate::ui::ui_size() + 4.0;
                let lh = Self::title_lh();
                for (i, l) in lines.iter().enumerate() {
                    self.tx.draw_ui_sized(cv, r.x, r.y + i as i32 * lh, &l.text, strong, clip, true, size);
                }
            }
            It::Badges(b) => {
                let mut x = r.x;
                for (t, tone) in b {
                    let col = self.gh_color(*tone);
                    x += self.gh_pill(cv, x, r.y + 2, t, col, clip) + 6;
                }
            }
            It::Dim(t) => {
                let lines = self.gh_wrapped(g, t, r.w, 2);
                for (i, l) in lines.iter().enumerate() {
                    self.tx.draw_ui(cv, r.x, r.y + i as i32 * 20, &l.text, dim, clip, false);
                }
            }
            It::Note(t, tone) => {
                let col = self.gh_color(*tone);
                let lines = self.gh_wrapped(g, t, r.w, 2);
                for (i, l) in lines.iter().enumerate() {
                    self.tx.draw_ui(cv, r.x, r.y + i as i32 * 20, &l.text, col, clip, false);
                }
            }
            It::Buttons(bs) => {
                let rows = self.gh_btn_rows(bs, r.w);
                let mut k = 0;
                for (ri, n) in rows.iter().enumerate() {
                    let y = r.y + ri as i32 * 34;
                    let mut x = r.x;
                    for b in &bs[k..k + n] {
                        x += self.gh_button(cv, g, x, y, b, clip) + 6;
                    }
                    k += n;
                }
            }
            It::Confirm(cf) => self.gh_draw_confirm(cv, g, cf, r, clip),
            It::Tabs(tabs) => {
                cv.fill_clip(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border, clip);
                let mut x = r.x;
                for (label, hit, on) in tabs {
                    let w = self.tx.ui_width(label, *on) + 24;
                    let tr = Rect::new(x, r.y, w, r.h - 1);
                    let hot = self.gh_hv(tr.intersect(&clip));
                    if hot && !*on {
                        cv.fill_round_clip(Rect::new(tr.x, tr.y + 2, tr.w, tr.h - 6), 6, strong.with_alpha(0x10), clip);
                    }
                    let col = if *on || hot { strong } else { dim };
                    self.tx.draw_ui(cv, tr.x + 12, tr.y + Self::gh_ty(tr.h), label, col, tr.intersect(&clip), *on);
                    if *on {
                        cv.fill_clip(Rect::new(tr.x + 6, r.bottom() - 2, tr.w - 12, 2), c.accent, clip);
                    }
                    g.hits.push((tr.intersect(&clip), hit.clone()));
                    x += w;
                }
            }
            It::Heading(t, n) => {
                self.tx.draw_ui(cv, r.x, r.y + 4, t, strong, clip, true);
                if *n > 0 {
                    let tw = self.tx.ui_width(t, true);
                    self.tx.draw_ui(cv, r.x + tw + 8, r.y + 4, &n.to_string(), dim, clip, false);
                }
                cv.fill_clip(Rect::new(r.x, r.bottom() - 4, r.w, 1), c.border.with_alpha(0x80), clip);
            }
            It::Md(b) => {
                let lines = self.gh_wrapped(g, b, r.w, 0);
                self.gh_draw_md(cv, &lines, r.x, r.y, r.w, clip);
            }
            It::Chips(labels) => {
                let rows = self.gh_chip_rows(labels, r.w);
                let mut k = 0;
                for (ri, n) in rows.iter().enumerate() {
                    let mut x = r.x;
                    let y = r.y + ri as i32 * 30;
                    for l in &labels[k..k + n] {
                        let w = self.gh_chips_w(l);
                        let pr = Rect::new(x, y, w, 24);
                        cv.fill_round_clip(pr, 12, c.widget_bg, clip);
                        cv.ring(pr.intersect(&clip), 12, c.border);
                        let (cr, cg, cb) = model::label_rgb(&l.color).unwrap_or((0x80, 0x80, 0x80));
                        cv.fill_round_clip(Rect::new(x + 8, y + 8, 8, 8), 4, Rgba(cr, cg, cb, 0xff), clip);
                        self.tx.draw_ui(cv, x + 22, y + Self::gh_ty(24), &l.name, strong, pr.intersect(&clip), false);
                        x += w + 6;
                    }
                    k += n;
                }
            }
            It::Kv(k, v) => {
                self.tx.draw_ui(cv, r.x, r.y + 2, k, dim, clip, false);
                let v = fit(&mut self.tx, v, r.w - 110, false);
                self.tx.draw_ui(cv, r.x + 100, r.y + 2, &v, strong, clip, false);
            }
            It::Comment(cm) => self.gh_draw_comment(cv, g, cm, r, clip),
            It::Row(row) => self.gh_draw_row(cv, g, row, r, clip),
            It::Diff(i) => self.gh_draw_diff(cv, g, *i, r, clip),
            It::Composer(bs) => self.gh_draw_composer(cv, g, bs, r, clip),
        }
    }

    fn gh_draw_confirm(&mut self, cv: &mut Canvas, g: &mut GhPanel, cf: &state::Confirm, r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        cv.fill_round_clip(r, 8, c.widget_bg, clip);
        cv.ring(r.intersect(&clip), 8, if cf.danger { c.error.with_alpha(0x80) } else { c.border });
        if cf.danger {
            cv.fill_clip(Rect::new(r.x, r.y + 6, 3, r.h - 12), c.error, clip);
        }
        let lines = self.gh_wrapped(g, &cf.text, r.w - 28, 2);
        for (i, l) in lines.iter().enumerate() {
            self.tx.draw_ui(cv, r.x + 14, r.y + 10 + i as i32 * 20, &l.text, c.tab_active_fg, clip, false);
        }
        let by = r.bottom() - 10 - 28;
        let yes = Btn {
            label: cf.yes.clone(),
            icon: "ui/check",
            hit: GhHit::Yes,
            style: if g.busy.is_some() {
                BtnStyle::Off
            } else if cf.danger {
                BtnStyle::Danger
            } else {
                BtnStyle::Primary
            },
        };
        let no = Btn { label: "Cancel".into(), icon: "ui/close", hit: GhHit::No, style: BtnStyle::Plain };
        let w = self.gh_button(cv, g, r.x + 14, by, &yes, clip);
        self.gh_button(cv, g, r.x + 14 + w + 6, by, &no, clip);
    }

    /// Markdown lines painted from (x, y), `w` wide.
    fn gh_draw_md(&mut self, cv: &mut Canvas, lines: &[MdLine], x: i32, y: i32, w: i32, clip: Rect) {
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let mut y = y;
        for l in lines {
            match l.kind {
                Kind::Blank => y += 8,
                Kind::Code => {
                    if y + CODE_H >= clip.y && y <= clip.bottom() {
                        cv.fill_clip(Rect::new(x, y, w, CODE_H), c.input_bg, clip);
                        let t: String = l
                            .text
                            .chars()
                            .take(((w - 16) / (self.tx.cell_w.max(1.0) as i32)).max(1) as usize)
                            .collect();
                        self.tx.draw_line(cv, x + 8, y + 1, &t, fg, Rect::new(x, y, w, CODE_H).intersect(&clip));
                    }
                    y += CODE_H;
                }
                Kind::H1 | Kind::H2 => {
                    let h = if l.kind == Kind::H1 { 28 } else { 24 };
                    if y + h >= clip.y && y <= clip.bottom() {
                        self.tx.draw_ui(cv, x, y + 4, &l.text, c.tab_active_fg, clip, true);
                    }
                    y += h;
                }
                Kind::Quote => {
                    if y + 20 >= clip.y && y <= clip.bottom() {
                        cv.fill_clip(Rect::new(x, y + 1, 3, 18), c.border, clip);
                        self.tx.draw_ui(cv, x + 12, y + 1, &l.text, c.tab_inactive_fg, clip, false);
                    }
                    y += 20;
                }
                Kind::Bullet | Kind::Text => {
                    if y + 20 >= clip.y && y <= clip.bottom() {
                        self.tx.draw_ui(cv, x, y + 1, &l.text, fg, clip, false);
                    }
                    y += 20;
                }
            }
        }
    }

    fn gh_draw_comment(&mut self, cv: &mut Canvas, g: &mut GhPanel, cm: &ghub::Comment, r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill_round_clip(r, 8, c.sidebar_bg, clip);
        cv.ring(r.intersect(&clip), 8, c.border);
        let head = Rect::new(r.x, r.y, r.w, 38);
        self.gh_avatar(cv, r.x + 10, r.y + 9, 20, &cm.author, clip);
        let name = if cm.author.is_empty() { "ghost" } else { &cm.author };
        self.tx.draw_ui(cv, r.x + 38, r.y + Self::gh_ty(38), name, strong, head.intersect(&clip), true);
        let nw = self.tx.ui_width(name, true);
        let verb = review_word(cm).map_or("commented", |(w, _)| w);
        let when = format!("{verb} {}", model::time_ago(&cm.when, now_secs()));
        self.tx.draw_ui(cv, r.x + 38 + nw + 8, r.y + Self::gh_ty(38), &when, dim, head.intersect(&clip), false);
        if let Some((w, tone)) = review_word(cm).filter(|(w, _)| *w != "reviewed") {
            let col = self.gh_color(tone);
            let pw = self.tx.ui_width(w, false) + 14;
            self.gh_pill(cv, r.right() - 12 - pw, r.y + 10, w, col, clip);
        }
        if cm.body.trim().is_empty() {
            return;
        }
        cv.fill_clip(Rect::new(r.x + 1, r.y + 37, r.w - 2, 1), c.border.with_alpha(0x80), clip);
        let lines = self.gh_wrapped(g, &cm.body, r.w - 28, 0);
        self.gh_draw_md(cv, &lines, r.x + 14, r.y + 38 + 8, r.w - 28, clip);
    }

    fn gh_draw_row(&mut self, cv: &mut Canvas, g: &mut GhPanel, row: &Row, r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let vis = r.intersect(&clip);
        let hot = self.gh_hv(vis);
        if hot {
            cv.fill_round_clip(r, 8, c.list_hover_bg, clip);
        }
        let mut x = r.x + row.indent + 8;
        if let Some((hit, open)) = &row.fold {
            let fr = Rect::new(x - 4, r.y + (r.h - 24) / 2, 24, 24);
            if self.gh_hv(fr.intersect(&clip)) {
                cv.fill_round_clip(fr, 6, strong.with_alpha(0x18), clip);
            }
            self.tx.icon(
                cv,
                if *open { "ui/chevron_down" } else { "ui/chevron_right" },
                x,
                r.y + (r.h - 12) / 2,
                12,
                dim,
                vis,
            );
            g.hits.push((fr.intersect(&clip), hit.clone()));
            x += 20;
        }
        let col = self.gh_color(row.tone);
        self.tx.icon(cv, row.icon, x, r.y + (r.h - 16) / 2, 16, col, vis);
        x += 26;
        let rw = if row.right.is_empty() { 0 } else { self.tx.ui_width(&row.right, false) + 16 };
        let room = (r.right() - x - rw - 8).max(30);
        let t = fit(&mut self.tx, &row.title, room, false);
        let two = !row.sub.is_empty();
        self.tx.draw_ui(cv, x, r.y + if two { 5 } else { Self::gh_ty(r.h) }, &t, strong, vis, false);
        if two {
            let s = fit(&mut self.tx, &row.sub, room, false);
            self.tx.draw_ui(cv, x, r.y + 24, &s, dim, vis, false);
        }
        if rw > 0 {
            self.tx.draw_ui(cv, r.right() - rw, r.y + Self::gh_ty(r.h), &row.right, dim, vis, false);
        }
        g.hits.push((vis, row.hit.clone()));
    }

    fn gh_draw_diff(&mut self, cv: &mut Canvas, g: &mut GhPanel, i: usize, r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let Some((n, k)) = g.diff_lines(i) else { return };
        let lh = crate::ui::line_h().ceil() as i32;
        let body = Rect::new(r.x, r.y, r.w, r.h - 6);
        cv.fill_round_clip(body, 6, c.input_bg, clip);
        cv.ring(body.intersect(&clip), 6, c.border);
        let Detail::Pr(v) = &g.detail else { return };
        let Some(files) = v.diff.ready() else { return };
        let file = &files[k];
        if file.binary && n == 0 {
            self.tx.draw_ui(cv, r.x + 12, r.y + 6, "Binary file not shown", c.tab_inactive_fg, clip, false);
            return;
        }
        if n == 0 {
            self.tx.draw_ui(
                cv,
                r.x + 12,
                r.y + 6,
                "No diff hunks available for this file.",
                c.tab_inactive_fg,
                clip,
                false,
            );
            return;
        }
        let cols = ((r.w - 24) / (self.tx.cell_w.max(1.0) as i32)).max(1) as usize;
        let mut y = r.y + 6;
        for l in file.lines.iter().take(DIFF_MAX) {
            if y + lh >= clip.y && y <= clip.bottom() {
                let (bg, mark, col) = match l.kind {
                    ghub::DiffKind::Add => (Some(c.diff_add), '+', fg),
                    ghub::DiffKind::Del => (Some(c.diff_del), '-', fg),
                    ghub::DiffKind::Hunk => (None, ' ', c.info),
                    ghub::DiffKind::Ctx => (None, ' ', fg),
                };
                let row = Rect::new(r.x + 1, y, r.w - 2, lh);
                if let Some(bg) = bg {
                    cv.fill_clip(row, bg, clip);
                }
                let text = if l.kind == ghub::DiffKind::Hunk {
                    l.text.clone()
                } else {
                    format!("{mark} {}", l.text.replace('\t', "    "))
                };
                let t: String = text.chars().take(cols).collect();
                self.tx.draw_line(cv, r.x + 10, y, &t, col, row.intersect(&clip));
            }
            y += lh;
        }
        if n > DIFF_MAX {
            let t = format!("… {} more lines (open the pull request on GitHub to see them)", n - DIFF_MAX);
            self.tx.draw_ui(cv, r.x + 10, y + 2, &t, c.tab_inactive_fg, clip, false);
        }
    }

    fn gh_draw_composer(&mut self, cv: &mut Canvas, g: &mut GhPanel, bs: &[Btn], r: Rect, clip: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let fg = self.theme.foreground;
        self.tx.draw_ui(cv, r.x, r.y + 4, "Write a comment", strong, clip, true);
        let bh = self.gh_composer_layout(g, r.w);
        let b = Rect::new(r.x, r.y + 28, r.w, bh);
        let on = g.composer_focus && g.focused;
        cv.card(b, 8, c.input_bg, if on { c.accent } else { c.border });
        g.composer_box = b;
        g.hits.push((b.intersect(&clip), GhHit::Composer));
        let lh = (crate::ui::ui_line().ceil() as i32).max(18);
        let (ox, oy) = (b.x + MSG_PAD_X, b.y + MSG_PAD_Y);
        let inner = Rect::new(b.x + 4, b.y + 4, b.w - 8, b.h - 8).intersect(&clip);
        if g.composer.text().is_empty() {
            self.tx.draw_ui(
                cv,
                ox,
                oy,
                "Leave a comment, or write a review note for Approve / Request changes…",
                dim,
                inner,
                false,
            );
        }
        let scroll = g.composer.scroll.round() as i32;
        let row_y = |k: usize| oy + k as i32 * lh - scroll;
        for (k, x0, x1) in g.composer.sel_spans() {
            let (a, z) = (x0.round() as i32, x1.round() as i32);
            cv.fill_clip(Rect::new(ox + a, row_y(k), (z - a).max(1), lh), self.theme.selection, inner);
        }
        for (k, l) in g.composer.lines().iter().enumerate() {
            let y = row_y(k);
            if y + lh < b.y || y > b.bottom() {
                continue;
            }
            let t = g.composer.line_text(l);
            self.tx.draw_ui(cv, ox, y, &t, fg, inner, false);
        }
        if on {
            let (k, x) = g.composer.caret();
            cv.fill_clip(Rect::new(ox + x.round() as i32, row_y(k) + 1, 1, lh - 2), strong, inner);
        }
        let rows = self.gh_btn_rows(bs, r.w);
        let mut k = 0;
        for (ri, n) in rows.iter().enumerate() {
            let y = b.bottom() + 10 + ri as i32 * 34;
            let mut x = r.x;
            for btn in &bs[k..k + n] {
                x += self.gh_button(cv, g, x, y, btn, clip) + 6;
            }
            k += n;
        }
    }
}
