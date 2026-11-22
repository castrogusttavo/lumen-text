//! Drawing of the first-run flow (see `app_onboarding` for the state and the keys): a full-window
//! surface with a header (brand, Skip), the current step sliding and fading in, and a footer
//! (Back / Continue, progress dots, key hints). Colors come from the theme, geometry from `ds`,
//! motion from `anim`.

use crate::{
    anim::{ease_out_cubic, fade, lerp_rgba, stagger},
    app::App,
    app_gitpanel::fit,
    app_onboarding::{FONT_MAX, FONT_MIN, Hit, KEYMAPS, Onboarding, SCALES, STEPS},
    ds::{ICON_L, ICON_S, R_CARD, R_CTRL, R_ROW, S1, S2, S3, S4, S5},
    theme::Rgba,
    ui::{Canvas, Rect, ui_line},
    ui_keycap::{draw_keys, keys_width, split_keys},
};

const HEADER_H: i32 = 56;
const FOOT_H: i32 = 84;
const BTN_H: i32 = 36;
/// Horizontal travel of a step sliding in or out.
const SLIDE_PX: f32 = 56.0;
const ROW_T: i32 = 34;
const CHOICE_H: i32 = 64;
const CONTENT_MAX: i32 = 720;
const COLUMN_MAX: i32 = 560;
/// Seconds of the backdrop fade-in.
const BACKDROP_SECS: f32 = 0.2;

/// Rows the theme list shows in a body of height `h` (heading above, a little air below).
pub(crate) fn theme_rows(h: i32) -> usize {
    ((h - HEADING_H - S3 - S2) / ROW_T).clamp(3, 9) as usize
}

/// Space the heading of a step takes (title, subtitle, gap).
const HEADING_H: i32 = 96;

fn line() -> i32 {
    ui_line() as i32
}

fn smooth(a: f32) -> f32 {
    let a = a.clamp(0.0, 1.0);
    a * a * (3.0 - 2.0 * a)
}

/// One step being drawn: where, how visible, and whether its controls are clickable.
#[derive(Clone, Copy)]
struct Layer {
    area: Rect,
    clip: Rect,
    a: f32,
    live: bool,
}

/// One row of the keymap / last-step lists.
struct Choice<'a> {
    icon: &'a str,
    title: &'a str,
    sub: &'a str,
    keys: Vec<String>,
    /// `Some(selected)` draws a radio at the right.
    radio: Option<bool>,
    /// Filled accent tile (the primary choice).
    strong: bool,
}

fn reg(st: &mut Onboarding, l: &Layer, r: Rect, h: Hit) {
    if l.live && r.w > 0 && r.h > 0 {
        st.rects.push((r, h));
    }
}

impl App {
    /// Draw the flow over the whole window. True while something animates.
    pub fn draw_onboarding(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let Some(mut st) = self.onboarding.take() else { return false };
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let bg = self.theme.background;
        let body = Rect::new(0, HEADER_H, sw, (sh - HEADER_H - FOOT_H).max(0));

        let (mx, my) = self.mouse;
        let hov = st.rects.iter().find(|(r, h)| h.glides() && r.contains(mx, my)).map(|(r, _)| *r);
        st.glide.aim(hov);
        st.retarget(&self.config.keymap_preset, theme_rows(body.h), false);
        let busy = st.advance(dt);
        st.rects.clear();

        let fadein = ease_out_cubic(st.t / BACKDROP_SECS);
        cv.fill(Rect::new(0, 0, sw, sh), fade(bg, fadein));

        let sv = st.slide.value.max(0.0);
        let (lo, hi) = (sv.floor() as usize, (sv.ceil() as usize).min(STEPS - 1));
        let cw = (sw - 2 * S5).clamp(280, CONTENT_MAX);
        for i in lo..=hi.max(lo) {
            let d = i as f32 - sv;
            let a = smooth(1.0 - d.abs());
            if a < 0.01 || i >= STEPS {
                continue;
            }
            let area = Rect::new((sw - cw) / 2 + (d * SLIDE_PX) as i32, body.y, cw, body.h);
            let l = Layer { area, clip: body, a: a * fadein, live: i == st.step };
            match i {
                0 => self.ob_welcome(cv, &st, l),
                1 => self.ob_theme(cv, &mut st, l),
                2 => self.ob_keymap(cv, &mut st, l),
                3 => self.ob_font(cv, &mut st, l),
                _ => self.ob_open(cv, &mut st, l),
            }
        }

        // Header and footer sit above the steps: whatever a step overflows into them is covered.
        cv.fill(Rect::new(0, 0, sw, HEADER_H), fade(bg, fadein));
        cv.fill(Rect::new(0, sh - FOOT_H, sw, FOOT_H), fade(bg, fadein));
        self.ob_header(cv, &mut st, sw);
        self.ob_footer(cv, &mut st, sw, sh, cw);
        self.onboarding = Some(st);
        busy
    }

    fn ob_header(&mut self, cv: &mut Canvas, st: &mut Onboarding, sw: i32) {
        let c = self.theme.chrome;
        let full = Rect::new(0, 0, sw, HEADER_H);
        let mh = 24;
        let mw = crate::brand::width_for(mh);
        crate::brand::draw_mark(cv, S5, (HEADER_H - mh) / 2, mh, full);
        self.tx.draw_ui(cv, S5 + mw + S2, (HEADER_H - line()) / 2, "Lumen", c.tab_active_fg, full, true);

        let label = "Skip tour";
        let keys = split_keys("Esc");
        let (tw, kw) = (self.tx.ui_width(label, false), keys_width(&mut self.tx, &keys));
        let w = tw + kw + S2 + 2 * S3;
        let r = Rect::new(sw - S5 - w, (HEADER_H - 30) / 2, w, 30);
        let hot = r.contains(self.mouse.0, self.mouse.1);
        cv.fill_round(r, R_CTRL + 2, if hot { c.list_hover_bg } else { c.widget_bg });
        cv.ring(r, R_CTRL + 2, c.border);
        let fg = if hot { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.draw_ui(cv, r.x + S3, r.y + (r.h - line()) / 2, label, fg, r, false);
        draw_keys(&mut self.tx, cv, r.x + S3 + tw + S2, r.y + (r.h - 18) / 2, &keys, c, 1.0);
        st.rects.push((r, Hit::Skip));
    }

    fn ob_footer(&mut self, cv: &mut Canvas, st: &mut Onboarding, sw: i32, sh: i32, cw: i32) {
        let c = self.theme.chrome;
        let y0 = sh - FOOT_H;
        let by = y0 + S4;
        let (left, right) = ((sw - cw) / 2, (sw + cw) / 2);

        let label = if st.step == 0 { "Let's go" } else { "Continue" };
        let nw = (self.tx.ui_width(label, true) + 2 * S5).max(120);
        if st.step + 1 < STEPS {
            let next = Rect::new(right - nw, by, nw, BTN_H);
            self.ob_btn(cv, next, label, true);
            st.rects.push((next, Hit::Next));
        }
        if st.step > 0 {
            let bw = self.tx.ui_width("Back", false) + 2 * S5;
            let back = Rect::new(left, by, bw, BTN_H);
            self.ob_btn(cv, back, "Back", false);
            st.rects.push((back, Hit::Back));
        }

        // Progress dots: the active one stretches into a pill while the slide passes it.
        let sv = st.slide.value;
        let widths: Vec<i32> =
            (0..STEPS).map(|i| 6 + (14.0 * (1.0 - (sv - i as f32).abs()).clamp(0.0, 1.0)) as i32).collect();
        let total: i32 = widths.iter().sum::<i32>() + (STEPS as i32 - 1) * S2;
        let mut x = (sw - total) / 2;
        let cy = by + BTN_H / 2;
        for (i, w) in widths.iter().enumerate() {
            let near = (1.0 - (sv - i as f32).abs()).clamp(0.0, 1.0);
            let col = lerp_rgba(fade(c.tab_inactive_fg, 0.45), c.accent, near);
            cv.fill_round(Rect::new(x, cy - 3, *w, 6), 3, col);
            st.rects.push((Rect::new(x - S1, cy - S3, w + 2 * S1, 2 * S3), Hit::Goto(i)));
            x += w + S2;
        }

        let hint = match st.step {
            0 => "Enter to continue",
            1 => "Up / Down to preview a theme",
            2 => "Up / Down or 1 2 3 to choose",
            3 => "Up / Down to change, Tab to switch",
            _ => "Up / Down to choose, Enter to confirm",
        };
        let hint =
            fit(&mut self.tx, &format!("{hint}   ·   Left / Right to move   ·   Esc to skip"), sw - 2 * S4, false);
        let hw = self.tx.ui_width(&hint, false);
        let full = Rect::new(0, y0, sw, FOOT_H);
        self.tx.draw_ui(cv, (sw - hw) / 2, by + BTN_H + S3, &hint, fade(c.tab_inactive_fg, 0.9), full, false);
    }

    fn ob_btn(&mut self, cv: &mut Canvas, r: Rect, label: &str, primary: bool) {
        let c = self.theme.chrome;
        let hot = r.contains(self.mouse.0, self.mouse.1);
        let fg = if primary {
            cv.fill_round(r, R_CTRL + 2, if hot { c.accent.shift(14) } else { c.accent });
            c.on_accent
        } else {
            cv.fill_round(r, R_CTRL + 2, if hot { c.list_hover_bg } else { c.widget_bg });
            cv.ring(r, R_CTRL + 2, c.border);
            c.tab_active_fg
        };
        let tw = self.tx.ui_width(label, true);
        self.tx.draw_ui(cv, r.x + (r.w - tw) / 2, r.y + (r.h - line()) / 2, label, fg, r, true);
    }

    /// Title and subtitle of a step; the body starts `HEADING_H` below the area top.
    fn ob_heading(&mut self, cv: &mut Canvas, l: &Layer, title: &str, sub: &str) -> i32 {
        let c = self.theme.chrome;
        let y = l.area.y + S4;
        self.tx.draw_ui_sized(cv, l.area.x, y, title, fade(self.theme.foreground, l.a), l.clip, true, 22.0);
        let sub = fit(&mut self.tx, sub, l.area.w, false);
        self.tx.draw_ui(cv, l.area.x, y + 36, &sub, fade(c.tab_inactive_fg, l.a), l.clip, false);
        l.area.y + HEADING_H
    }

    /// Step 1: the welcome, with three reasons to stay.
    fn ob_welcome(&mut self, cv: &mut Canvas, st: &Onboarding, l: Layer) {
        let c = self.theme.chrome;
        let narrow = l.area.w < 560;
        let feats = [
            ("ui/keyboard", "Keyboard first", "Every action is a command"),
            ("ui/bolt_outlined", "Native and fast", "No browser engine inside"),
            ("ui/terminal", "Your own AI CLI", "A real terminal, untouched"),
        ];
        let chip_h = if narrow { 52 } else { 104 };
        let chips = if narrow { 3 * chip_h + 2 * S2 } else { chip_h };
        let total = 64 + S4 + 42 + S1 + line() + S5 + chips + S5 + line();
        let mut y = l.area.y + ((l.area.h - total) / 2).max(0);
        let cx = l.area.x + l.area.w / 2;
        let mut n = 0usize;
        let mut item = || {
            let k = l.a * stagger(st.t, n, 0.07, 0.4);
            n += 1;
            (k, ((1.0 - k) * 12.0) as i32)
        };

        let (k, dy) = item();
        let mw = crate::brand::width_for(64);
        crate::brand::draw_mark(cv, cx - mw / 2, y + dy, 64, l.clip);
        if k < 1.0 {
            cv.fill(Rect::new(cx - mw / 2 - 2, y + dy - 2, mw + 4, 68), fade(self.theme.background, 1.0 - k));
        }
        y += 64 + S4;

        let (k, dy) = item();
        let title = "Welcome to Lumen";
        let tw = self.tx.ui_width_sized(title, true, 30.0);
        self.tx.draw_ui_sized(cv, cx - tw / 2, y + dy, title, fade(self.theme.foreground, k), l.clip, true, 30.0);
        y += 42 + S1;

        let (k, dy) = item();
        let tag = fit(&mut self.tx, "A fast, native editor built for the way you code.", l.area.w, false);
        let tw = self.tx.ui_width(&tag, false);
        self.tx.draw_ui(cv, cx - tw / 2, y + dy, &tag, fade(c.tab_inactive_fg, k), l.clip, false);
        y += line() + S5;

        let cw3 = (l.area.w - 2 * S3) / 3;
        for (i, (icon, head, desc)) in feats.iter().enumerate() {
            let (k, dy) = item();
            let r = if narrow {
                Rect::new(l.area.x, y + i as i32 * (chip_h + S2) + dy, l.area.w, chip_h)
            } else {
                Rect::new(l.area.x + i as i32 * (cw3 + S3), y + dy, cw3, chip_h)
            };
            cv.fill_round(r, R_CARD, fade(c.sidebar_bg, k));
            cv.ring(r, R_CARD, fade(c.border, k));
            let tile = Rect::new(r.x + S3, if narrow { r.y + (r.h - 32) / 2 } else { r.y + S3 }, 32, 32);
            cv.fill_round(tile, R_CTRL + 2, fade(c.accent.with_alpha(0x26), k));
            self.tx.icon(
                cv,
                icon,
                tile.x + (32 - ICON_L) / 2,
                tile.y + (32 - ICON_L) / 2,
                ICON_L,
                fade(c.accent_text, k),
                l.clip,
            );
            let (tx0, ty) =
                if narrow { (tile.right() + S3, r.y + (r.h - 2 * line()) / 2) } else { (r.x + S3, tile.bottom() + S2) };
            let avail = r.right() - S3 - tx0;
            let head = fit(&mut self.tx, head, avail, true);
            self.tx.draw_ui(cv, tx0, ty, &head, fade(c.tab_active_fg, k), l.clip, true);
            let desc = fit(&mut self.tx, desc, avail, false);
            self.tx.draw_ui(cv, tx0, ty + line(), &desc, fade(c.tab_inactive_fg, k), l.clip, false);
        }
        y += chips + S5;

        let (k, dy) = item();
        let note = fit(&mut self.tx, "Takes under a minute. Change anything later in Settings.", l.area.w, false);
        let tw = self.tx.ui_width(&note, false);
        self.tx.draw_ui(cv, cx - tw / 2, y + dy, &note, fade(c.tab_inactive_fg, k), l.clip, false);
    }

    /// Step 2: pick a theme; the app behind and the preview next to the list follow live.
    fn ob_theme(&mut self, cv: &mut Canvas, st: &mut Onboarding, l: Layer) {
        let c = self.theme.chrome;
        let y0 = self.ob_heading(cv, &l, "Pick a theme", "It applies as you move. Arrow keys or click to choose.");
        let rows = theme_rows(l.area.h);
        let two = l.area.w >= 560;
        let lw = if two { 240 } else { l.area.w };
        let lr = Rect::new(l.area.x, y0, lw, rows as i32 * ROW_T + S2);
        cv.fill_round(lr, R_CARD, fade(c.sidebar_bg, l.a));
        cv.ring(lr, R_CARD, fade(c.border, l.a));
        let inner = lr.inset(S1);
        let scroll = st.scroll.value;

        if l.live
            && let Some((g, ga)) = st.glide.now()
        {
            cv.fill_round_clip(g, R_ROW, fade(c.list_hover_bg, ga * l.a), inner);
        }
        let sy = inner.y + ((st.selg[0].value - scroll) * ROW_T as f32).round() as i32;
        let sr = Rect::new(inner.x, sy, inner.w, ROW_T - 2);
        cv.fill_round_clip(sr, R_ROW, fade(c.accent.with_alpha(0x30), l.a), inner);
        cv.fill_round_clip(Rect::new(sr.x + 2, sr.y + 8, 3, sr.h - 16), 1, fade(c.accent, l.a), inner);

        let first = scroll.floor().max(0.0) as usize;
        for i in first..(first + rows + 2).min(st.names.len()) {
            let y = inner.y + ((i as f32 - scroll) * ROW_T as f32).round() as i32;
            let rr = Rect::new(inner.x, y, inner.w, ROW_T - 2);
            if rr.bottom() < inner.y || rr.y > inner.bottom() {
                continue;
            }
            let on = i == st.sel;
            let name = fit(&mut self.tx, &st.names[i], rr.w - 2 * S3 - ICON_S - S2, on);
            let fg = if on { c.tab_active_fg } else { c.sidebar_fg };
            self.tx.draw_ui(cv, rr.x + S3 + S1, rr.y + (rr.h - line()) / 2, &name, fade(fg, l.a), inner, on);
            if on {
                self.tx.icon(
                    cv,
                    "ui/check",
                    rr.right() - S3 - ICON_S,
                    rr.y + (rr.h - ICON_S) / 2,
                    ICON_S,
                    fade(c.accent_text, l.a),
                    inner,
                );
            }
            reg(st, &l, rr.intersect(&inner), Hit::Theme(i));
        }
        let len = st.names.len();
        if len > rows {
            let track = (lr.h - 2 * S2) as f32;
            let th = (track * rows as f32 / len as f32).max(16.0);
            let ty = lr.y as f32 + S2 as f32 + (track - th) * scroll / (len - rows) as f32;
            cv.fill_round(Rect::new(lr.right() - 5, ty as i32, 2, th as i32), 1, fade(c.tab_inactive_fg, 0.35 * l.a));
        }

        if two {
            let px = lr.right() + S5;
            let pr = Rect::new(px, y0, l.area.right() - px, lr.h);
            let name = st.names.get(st.sel).cloned().unwrap_or_default();
            self.ob_preview(cv, pr, &name, &l);
        }
    }

    /// A miniature editor in the active theme: tabs, sidebar, code, status bar.
    fn ob_preview(&mut self, cv: &mut Canvas, r: Rect, name: &str, l: &Layer) {
        let c = self.theme.chrome;
        let th = &self.theme;
        let a = l.a;
        let (strip, status) = (24, 18);
        let sbw = (r.w / 4).clamp(48, 88);
        cv.fill_round(r, R_CARD, fade(c.status_bg, a));
        cv.fill_round(Rect::new(r.x, r.y, r.w, strip + R_CARD), R_CARD, fade(c.tabbar_bg, a));
        let ed = Rect::new(r.x, r.y + strip, r.w, r.h - strip - status);
        cv.fill(ed, fade(th.background, a));
        cv.fill(Rect::new(ed.x, ed.y, sbw, ed.h), fade(c.sidebar_bg, a));
        cv.ring(r, R_CARD, fade(c.border, a));

        // Tabs.
        let tab = Rect::new(r.x + sbw + S2, r.y + 4, 76, strip - 4);
        cv.fill_round(tab, R_CTRL, fade(c.tab_active_bg, a));
        self.tx.draw_ui_sized(cv, tab.x + S2, tab.y + 3, "main.rs", fade(c.tab_active_fg, a), tab, false, 11.0);
        self.tx.draw_ui_sized(cv, tab.right() + S3, tab.y + 3, "lib.rs", fade(c.tab_inactive_fg, a), r, false, 11.0);

        // Sidebar rows.
        for (i, w) in [44, 56, 38, 50, 30].into_iter().enumerate() {
            let row = Rect::new(ed.x + S2, ed.y + S2 + i as i32 * 16, w.min(sbw - 2 * S2), 6);
            if i == 1 {
                cv.fill_round(Rect::new(ed.x + S1, row.y - 4, sbw - S2, 14), R_CTRL, fade(c.list_active_bg, a));
            }
            cv.fill_round(row, 3, fade(c.sidebar_fg.with_alpha(0x80), a));
        }

        // Code.
        let kinds = [c.accent_text, c.git_added, c.warning, c.info, th.foreground.with_alpha(0xC0), c.error];
        let lines: [(i32, &[(i32, usize)]); 7] = [
            (0, &[(34, 0), (60, 4), (16, 4)]),
            (1, &[(40, 1), (10, 4), (70, 2)]),
            (1, &[(28, 3), (52, 4), (36, 1)]),
            (1, &[(64, 4), (24, 0)]),
            (2, &[(48, 2), (30, 4)]),
            (1, &[(14, 4)]),
            (0, &[(10, 4)]),
        ];
        let cx = ed.x + sbw + S3;
        let cw = r.right() - cx - S3;
        for (i, (indent, toks)) in lines.iter().enumerate() {
            let y = ed.y + S2 + i as i32 * 16;
            if y + 8 > ed.bottom() {
                break;
            }
            if i == 3 {
                cv.fill(Rect::new(ed.x + sbw, y - 4, ed.w - sbw, 16), fade(th.line_highlight, a));
            }
            cv.fill_round(Rect::new(cx, y + 1, 8, 4), 2, fade(th.line_number, a));
            let mut x = cx + 20 + indent * 14;
            for (w, kind) in toks.iter() {
                let w = (*w).min(cx + cw - x);
                if w <= 0 {
                    break;
                }
                cv.fill_round(Rect::new(x, y, w, 6), 3, fade(kinds[*kind], a));
                x += w + 6;
            }
            if i == 3 {
                cv.fill(Rect::new(x.min(cx + cw), y - 2, 2, 12), fade(th.cursor, a));
            }
        }

        // Status bar with the theme name.
        let sy = r.bottom() - status;
        cv.fill_round(Rect::new(r.x + S3, sy + 6, 6, 6), 3, fade(c.accent, a));
        let label = fit(&mut self.tx, name, r.w - 2 * S3 - 14, false);
        let lw = self.tx.ui_width_sized(&label, false, 11.0);
        self.tx.draw_ui_sized(cv, r.right() - S3 - lw, sy + 2, &label, fade(c.status_fg, a), r, false, 11.0);
    }

    /// Selection card behind the current row of a choice list (glides between rows).
    fn ob_select_card(&mut self, cv: &mut Canvas, st: &Onboarding, l: &Layer, r: Rect) {
        let c = self.theme.chrome;
        if l.live
            && let Some((g, ga)) = st.glide.now()
        {
            cv.fill_round(g, R_CARD, fade(c.list_hover_bg, ga * l.a));
        }
        cv.fill_round(r, R_CARD, fade(c.accent.with_alpha(0x1c), l.a));
        cv.ring(r, R_CARD, fade(c.accent, l.a));
    }

    /// Contents of one choice row: icon tile, title and sub line, keycaps, radio.
    fn ob_choice(&mut self, cv: &mut Canvas, l: &Layer, r: Rect, ch: &Choice) {
        let c = self.theme.chrome;
        let a = l.a;
        let tile = Rect::new(r.x + S4, r.y + (r.h - 40) / 2, 40, 40);
        let (fill, ic) = if ch.strong { (c.accent, c.on_accent) } else { (c.widget_bg, c.tab_active_fg) };
        cv.fill_round(tile, R_CARD - 2, fade(fill, a));
        if !ch.strong {
            cv.ring(tile, R_CARD - 2, fade(c.border, a));
        }
        self.tx.icon(cv, ch.icon, tile.x + (40 - ICON_L) / 2, tile.y + (40 - ICON_L) / 2, ICON_L, fade(ic, a), l.clip);

        let mut rx = r.right() - S4;
        if let Some(on) = ch.radio {
            let rd = Rect::new(rx - 20, r.y + (r.h - 20) / 2, 20, 20);
            if on {
                cv.fill_round(rd, 10, fade(c.accent, a));
                self.tx.icon(cv, "ui/check", rd.x + 3, rd.y + 3, 14, fade(c.on_accent, a), l.clip);
            } else {
                cv.ring(rd, 10, fade(c.tab_inactive_fg, 0.6 * a));
            }
            rx = rd.x - S3;
        }
        if !ch.keys.is_empty() {
            let kw = keys_width(&mut self.tx, &ch.keys);
            draw_keys(&mut self.tx, cv, rx - kw, r.y + (r.h - 18) / 2, &ch.keys, c, a);
            rx -= kw + S3;
        }
        let tx0 = tile.right() + S3;
        let avail = (rx - tx0).max(20);
        let top = r.y + (r.h - 2 * line() - S1) / 2;
        let title = fit(&mut self.tx, ch.title, avail, true);
        self.tx.draw_ui(cv, tx0, top, &title, fade(c.tab_active_fg, a), l.clip, true);
        let sub = fit(&mut self.tx, ch.sub, avail, false);
        self.tx.draw_ui(cv, tx0, top + line() + S1, &sub, fade(c.tab_inactive_fg, a), l.clip, false);
    }

    /// Step 3: keymap preset.
    fn ob_keymap(&mut self, cv: &mut Canvas, st: &mut Onboarding, l: Layer) {
        let c = self.theme.chrome;
        let y0 = self.ob_heading(
            cv,
            &l,
            "Choose a keymap",
            "Familiar shortcuts from day one. Change them any time in Settings.",
        );
        let w = l.area.w.min(COLUMN_MAX);
        let pitch = CHOICE_H + S2;
        for i in 0..KEYMAPS.len() {
            cv.ring(Rect::new(l.area.x, y0 + i as i32 * pitch, w, CHOICE_H), R_CARD, fade(c.border, l.a));
        }
        let sel = Rect::new(l.area.x, y0 + (st.selg[1].value * pitch as f32).round() as i32, w, CHOICE_H);
        self.ob_select_card(cv, st, &l, sel);
        for (i, (id, label, desc, icon)) in KEYMAPS.iter().enumerate() {
            let r = Rect::new(l.area.x, y0 + i as i32 * pitch, w, CHOICE_H);
            let on = self.config.keymap_preset == *id;
            let ch = Choice {
                icon,
                title: label,
                sub: desc,
                keys: vec![(i + 1).to_string()],
                radio: Some(on),
                strong: false,
            };
            self.ob_choice(cv, &l, r, &ch);
            reg(st, &l, r, Hit::Keymap(*id));
        }
    }

    /// Step 4: editor font size (with a live sample) and interface scale.
    fn ob_font(&mut self, cv: &mut Canvas, st: &mut Onboarding, l: Layer) {
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let y0 = self.ob_heading(
            cv,
            &l,
            "Size things to your eyes",
            "Editor font size and interface scale. Tab moves between the two.",
        );
        let (x0, w) = (l.area.x, l.area.w.min(COLUMN_MAX));
        let size = self.config.buffer_font_size;

        // Font size stepper.
        self.tx.draw_ui(cv, x0, y0 + (32 - line()) / 2, "Editor font size", fade(fg, l.a), l.clip, true);
        let plus = Rect::new(x0 + w - 32, y0, 32, 32);
        let val = Rect::new(plus.x - S2 - 64, y0, 64, 32);
        let minus = Rect::new(val.x - S2 - 32, y0, 32, 32);
        cv.fill_round(val, R_CTRL + 2, fade(c.input_bg, l.a));
        cv.ring(val, R_CTRL + 2, fade(c.border, l.a));
        let txt = format!("{size:.0}px");
        let tw = self.tx.ui_width(&txt, true);
        self.tx.draw_ui(cv, val.x + (val.w - tw) / 2, val.y + (val.h - line()) / 2, &txt, fade(fg, l.a), l.clip, true);
        self.ob_btn(cv, minus, "-", false);
        self.ob_btn(cv, plus, "+", false);
        reg(st, &l, minus, Hit::Size((size - 1.0).max(FONT_MIN)));
        reg(st, &l, plus, Hit::Size((size + 1.0).min(FONT_MAX)));
        if st.focus == 0 {
            cv.ring(
                Rect::new(minus.x - 3, y0 - 3, plus.right() - minus.x + 6, 38),
                R_CTRL + 4,
                fade(c.accent_text, l.a),
            );
        }

        // Live sample in the real editor font.
        let sb = Rect::new(x0, y0 + 32 + S3, w, 104);
        cv.fill_round(sb, R_CARD, fade(c.input_bg, l.a));
        cv.ring(sb, R_CARD, fade(c.border, l.a));
        let lh = crate::ui::line_h().ceil() as i32;
        let inner = sb.inset(S1);
        let f = |col: Rgba| fade(col, l.a);
        let rows: [Vec<(String, Rgba)>; 3] = [
            vec![("fn ".into(), f(c.accent_text)), ("greet(name: &str) {".into(), f(fg))],
            vec![("    println!(\"Hello, {name}\");".into(), f(fg))],
            vec![("}".into(), f(fg))],
        ];
        for (i, pieces) in rows.iter().enumerate() {
            self.tx.draw(cv, sb.x + S4, sb.y + S3 + i as i32 * lh, pieces, inner);
        }

        // Interface scale.
        let y1 = sb.bottom() + S5;
        self.tx.draw_ui(cv, x0, y1, "Interface scale", fade(fg, l.a), l.clip, true);
        let pw = ((w - 4 * S2) / SCALES.len() as i32).min(100);
        let py = y1 + line() + S2;
        for (i, (label, v)) in SCALES.iter().enumerate() {
            let r = Rect::new(x0 + i as i32 * (pw + S2), py, pw, BTN_H - 2);
            self.ob_btn(cv, r, label, (self.config.ui_scale - v).abs() < 0.01);
            reg(st, &l, r, Hit::Scale(*v));
        }
        if st.focus == 1 {
            let row = Rect::new(x0 - 3, py - 3, SCALES.len() as i32 * (pw + S2) - S2 + 6, BTN_H + 4);
            cv.ring(row, R_CTRL + 4, fade(c.accent_text, l.a));
        }
    }

    /// Step 5: open a project or start empty.
    fn ob_open(&mut self, cv: &mut Canvas, st: &mut Onboarding, l: Layer) {
        let c = self.theme.chrome;
        let y0 = self.ob_heading(
            cv,
            &l,
            "Open your first project",
            "Start from a folder, or begin empty. Switch projects any time from the title bar.",
        );
        let w = l.area.w.min(COLUMN_MAX);
        let pitch = CHOICE_H + S2;
        let count = st.pick_count();
        let row = |i: usize| Rect::new(l.area.x, y0 + i as i32 * pitch, w, CHOICE_H);
        for i in 0..count {
            cv.ring(row(i), R_CARD, fade(c.border, l.a));
        }
        let sel = Rect::new(l.area.x, y0 + (st.selg[2].value * pitch as f32).round() as i32, w, CHOICE_H);
        self.ob_select_card(cv, st, &l, sel);

        // (what a click does, icon, title, sub line, primary tile)
        let mut items: Vec<(Hit, &str, String, String, bool)> = vec![(
            Hit::OpenFolder,
            "ui/folder_open",
            "Open a folder".into(),
            "Browse your disk for a project".into(),
            true,
        )];
        for (i, p) in st.recents.iter().enumerate() {
            let (name, dir) = (crate::app_topbar::project_name(p), crate::app_topbar::tilde(p));
            items.push((Hit::Recent(i), "ui/folder", name, dir, false));
        }
        items.push((
            Hit::Done,
            "ui/arrow_right",
            "Start with an empty window".into(),
            "Everything stays one command away".into(),
            false,
        ));

        let shortcut = self.shortcut_text("ProjectMenu").map(|s| split_keys(&s)).unwrap_or_default();
        for (i, (hit, icon, title, sub, strong)) in items.into_iter().enumerate() {
            let keys = match (st.pick == i, i) {
                (true, _) => vec!["Enter".to_string()],
                (false, 0) => shortcut.clone(),
                _ => Vec::new(),
            };
            let ch = Choice { icon, title: &title, sub: &sub, keys, radio: None, strong };
            self.ob_choice(cv, &l, row(i), &ch);
            reg(st, &l, row(i), hit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_rows_follow_the_window_height() {
        assert_eq!(theme_rows(100), 3);
        assert_eq!(theme_rows(2000), 9);
        let mid = theme_rows(400);
        assert!((3..=9).contains(&mid));
        assert!(theme_rows(500) >= mid);
    }

    #[test]
    fn smooth_is_clamped_and_monotonic() {
        assert_eq!(smooth(-1.0), 0.0);
        assert_eq!(smooth(2.0), 1.0);
        assert!(smooth(0.25) < smooth(0.75));
    }
}
