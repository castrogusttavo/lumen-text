//! The page frame, the list column (section tabs, repository, filter, grouped rows), the messages
//! for "no gh / not logged in / no repository" and the dropdown menus.

use super::*;
use crate::{app_aipanel::SLOT_H, scrollreg::Reg};
use ghub::{IssueItem, Notif, PrItem, RunItem};
use items::{BtnStyle, phase_icon};
use model::{PrStatus, short_time};

pub(super) const TABS_H: i32 = 40;
const REPO_H: i32 = 32;
const TOOL_H: i32 = 38;
pub(super) const ROW_H: i32 = 52;
pub(super) const GROUP_H: i32 = 28;
/// Window width from which the list and the detail sit side by side.
pub(super) const TWO_PANE_MIN_W: i32 = 760;
const MERGED: Rgba = Rgba(0xa3, 0x71, 0xf7, 0xff);

/// One line of the list: a group header or an item (index into the source list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LRow {
    Group(usize),
    Item(usize),
}

pub(super) fn list_width(w: i32) -> i32 {
    if w >= TWO_PANE_MIN_W { (w * 36 / 100).clamp(300, 420) } else { w }
}

impl App {
    pub(super) fn gh_hv(&self, r: Rect) -> bool {
        self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)
    }

    /// The GitHub tab under the "Git | GitHub | AI" switch (right-hand slot).
    pub fn gh_rect(&self) -> Rect {
        let s = self.slot_rect();
        Rect::new(s.x, s.y + SLOT_H, s.w, (s.h - SLOT_H).max(0))
    }

    pub(super) fn gh_color(&self, t: Tone) -> Rgba {
        let c = self.theme.chrome;
        match t {
            Tone::Success => c.git_added,
            Tone::Error => c.error,
            Tone::Warning => c.warning,
            Tone::Accent => c.accent_text,
            Tone::Merged => MERGED,
            Tone::Muted => c.tab_inactive_fg,
        }
    }

    /// Top of a text line `lh` px high so it sits in the middle.
    pub(super) fn gh_ty(h: i32) -> i32 {
        (h - crate::ui::ui_line().round() as i32) / 2
    }

    /// A small rounded label; returns its width.
    pub(super) fn gh_pill(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, col: Rgba, clip: Rect) -> i32 {
        let w = self.tx.ui_width(text, false) + 14;
        let r = Rect::new(x, y, w, 18);
        cv.fill_round_clip(r, 9, col.with_alpha(0x26), clip);
        self.tx.draw_ui(cv, x + 7, y + Self::gh_ty(18), text, col, clip.intersect(&r), false);
        w
    }

    /// A round avatar with the first letter of a login.
    pub(super) fn gh_avatar(&mut self, cv: &mut Canvas, x: i32, y: i32, size: i32, login: &str, clip: Rect) {
        let (r, g, b) = model::hue_rgb(model::avatar_hue(login));
        let rect = Rect::new(x, y, size, size);
        cv.fill_round_clip(rect, size / 2, Rgba(r, g, b, 0xff), clip);
        let ch = model::initial(login);
        let w = self.tx.ui_width(&ch, true);
        self.tx.draw_ui(cv, x + (size - w) / 2, y + Self::gh_ty(size), &ch, Rgba(255, 255, 255, 255), rect, true);
    }

    pub(super) fn gh_icon_btn(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect, icon: &str, hit: GhHit, on: bool) {
        let c = self.theme.chrome;
        let hot = self.gh_hv(r);
        if hot || on {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(if on { 0x26 } else { 0x1c }));
        }
        let tint = if hot || on { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, r);
        g.hits.push((r, hit));
    }

    /// A button (icon and label); returns its width. Disabled ones answer clicks with nothing.
    pub(super) fn gh_button(
        &mut self,
        cv: &mut Canvas,
        g: &mut GhPanel,
        x: i32,
        y: i32,
        b: &items::Btn,
        clip: Rect,
    ) -> i32 {
        let c = self.theme.chrome;
        let lw = self.tx.ui_width(&b.label, false);
        let w = 12 + 16 + 6 + lw + 12;
        let r = Rect::new(x, y, w, 28);
        let hot = self.gh_hv(r) && self.gh_hv(clip) && b.style != BtnStyle::Off;
        let (fill, fg, edge) = match b.style {
            BtnStyle::Primary => (if hot { c.accent.shift(14) } else { c.accent }, c.on_accent, c.accent),
            BtnStyle::Danger => (c.error.with_alpha(if hot { 0x30 } else { 0x1a }), c.error, c.error.with_alpha(0x60)),
            BtnStyle::Plain => (if hot { c.list_hover_bg } else { c.widget_bg }, c.tab_active_fg, c.border),
            BtnStyle::Off => (c.widget_bg.with_alpha(0x80), c.tab_inactive_fg.with_alpha(0xa0), c.border),
        };
        cv.fill_round_clip(r, 8, fill, clip);
        cv.ring(r.intersect(&clip), 8, edge);
        self.tx.icon(cv, b.icon, r.x + 12, r.y + 6, 16, fg, r.intersect(&clip));
        self.tx.draw_ui(cv, r.x + 34, r.y + Self::gh_ty(28), &b.label, fg, r.intersect(&clip), false);
        let hit = if b.style == BtnStyle::Off { GhHit::Noop } else { b.hit.clone() };
        g.hits.push((r.intersect(&clip), hit));
        w
    }

    // ───────────── the page ─────────────

    pub fn draw_ghpanel(&mut self, cv: &mut Canvas) {
        if !self.gh_open() {
            return;
        }
        let Some(mut g) = self.gh.take() else { return };
        g.hits.clear();
        let r = self.gh_rect();
        let c = self.theme.chrome;
        cv.fill(r, self.theme.background);
        cv.fill(Rect::new(r.x, r.y, 1, r.h), c.border);
        let inner = Rect::new(r.x + 1, r.y, r.w - 1, r.h);
        match g.env.clone() {
            Env::Ready(_) => self.gh_draw_main(cv, &mut g, inner),
            env => self.gh_draw_env(cv, &mut g, inner, &env),
        }
        self.gh_draw_menu(cv, &mut g, inner);
        self.gh = Some(g);
    }

    fn gh_draw_main(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect) {
        let c = self.theme.chrome;
        let two = r.w >= TWO_PANE_MIN_W;
        let lw = list_width(r.w);
        let show_list = two || matches!(g.detail, Detail::None);
        if show_list {
            let lr = Rect::new(r.x, r.y, lw, r.h);
            cv.fill(lr, c.sidebar_bg);
            self.gh_draw_list(cv, g, lr);
        }
        if two {
            cv.fill(Rect::new(r.x + lw, r.y, 1, r.h), c.border);
            let dr = Rect::new(r.x + lw + 1, r.y, r.w - lw - 1, r.h);
            self.gh_draw_detail(cv, g, dr, true);
        } else if !show_list {
            self.gh_draw_detail(cv, g, r, false);
        }
    }

    /// The message of a page that cannot work yet, and what to do about it.
    fn gh_draw_env(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect, env: &Env) {
        let c = self.theme.chrome;
        let (title, lines, action): (&str, Vec<String>, Option<(&str, GhHit)>) = match env {
            Env::Checking => ("Checking GitHub…", vec![], None),
            Env::NoGh => (
                "GitHub CLI not found",
                vec![
                    "The GitHub page talks to GitHub through the `gh` command, which is not installed.".into(),
                    "Install it from https://cli.github.com, then run `gh auth login` in a terminal.".into(),
                ],
                Some(("Get GitHub CLI", GhHit::Link("https://cli.github.com".into()))),
            ),
            Env::NoAuth => (
                "GitHub not authenticated",
                vec![
                    "`gh` is installed but not logged in to github.com.".into(),
                    "Run `gh auth login` in a terminal (or set GH_TOKEN), then try again.".into(),
                ],
                Some(("Try again", GhHit::Retry)),
            ),
            Env::NoRepo => (
                "No GitHub repository",
                vec![
                    "This folder is not a Git repository with a GitHub remote.".into(),
                    "Open a project cloned from GitHub, or add one with `git remote add origin <url>`.".into(),
                ],
                Some(("Check again", GhHit::Retry)),
            ),
            Env::Failed(e) => ("GitHub is unavailable", vec![e.clone()], Some(("Try again", GhHit::Retry))),
            Env::Ready(_) => return,
        };
        let cx = r.x + r.w / 2;
        let mut y = r.y + (r.h / 3).max(40);
        let badge = Rect::new(cx - 24, y, 48, 48);
        cv.fill_round(badge, 24, c.tab_active_fg.with_alpha(0x14));
        self.tx.icon(cv, "ui/github", badge.x + 12, badge.y + 12, 24, c.tab_inactive_fg, r);
        y += 64;
        let w = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, cx - w / 2, y, title, c.tab_active_fg, r, true);
        y += 28;
        for l in &lines {
            for part in md::wrap(l, (r.w - 64).clamp(120, 420), &mut |s| self.tx.ui_width(s, false)) {
                let pw = self.tx.ui_width(&part, false);
                self.tx.draw_ui(cv, cx - pw / 2, y, &part, c.tab_inactive_fg, r, false);
                y += 20;
            }
        }
        if let Some((label, hit)) = action {
            y += 12;
            let b = items::Btn { label: label.into(), icon: "ui/rotate_cw", hit, style: BtnStyle::Plain };
            let bw = 12 + 16 + 6 + self.tx.ui_width(&b.label, false) + 12;
            self.gh_button(cv, g, cx - bw / 2, y, &b, r);
        }
    }

    // ───────────── the list column ─────────────

    fn gh_draw_list(&mut self, cv: &mut Canvas, g: &mut GhPanel, r: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        // Section tabs and refresh.
        let tabs = Rect::new(r.x, r.y, r.w, TABS_H);
        cv.fill(Rect::new(r.x, tabs.bottom() - 1, r.w, 1), c.border);
        let refresh = Rect::new(r.right() - 8 - 28, r.y + 6, 28, 28);
        let mut x = r.x + 8;
        let room = refresh.x - 4 - x;
        // Labels of every tab need this much; when they do not fit only the active one has its label.
        let need: i32 = Section::ALL.iter().map(|s| 10 + 16 + 6 + self.tx.ui_width(s.title(), true) + 10 + 4).sum();
        for s in Section::ALL {
            let on = g.section == s;
            let label = on || need <= room;
            let lw = if label { self.tx.ui_width(s.title(), on) } else { 0 };
            let w = 10 + 16 + if label { 6 + lw } else { 0 } + 10;
            let tr = Rect::new(x, r.y + 6, w, 28);
            let hot = self.gh_hv(tr);
            if on {
                cv.fill_round(tr, 8, strong.with_alpha(0x1e));
            } else if hot {
                cv.fill_round(tr, 8, strong.with_alpha(0x12));
            }
            let col = if on || hot { strong } else { dim };
            self.tx.icon(cv, s.icon(), tr.x + 10, tr.y + 6, 16, col, tr);
            if label {
                self.tx.draw_ui(cv, tr.x + 32, tr.y + Self::gh_ty(28), s.title(), col, tr, on);
            }
            // Unread notifications on the Inbox tab.
            if s == Section::Inbox && !on {
                let n = g.inbox.items.as_ref().map_or(0, |v| v.iter().filter(|n| n.unread).count());
                if n > 0 {
                    cv.fill_round(Rect::new(tr.x + 22, tr.y + 5, 8, 8), 4, c.accent);
                }
            }
            g.hits.push((tr, GhHit::Section(s)));
            x += w + 4;
        }
        let busy = g.loading_now();
        self.gh_icon_btn(cv, g, refresh, "ui/rotate_cw", GhHit::Refresh, busy);

        // Repository row.
        let mut y = tabs.bottom();
        let repo = Rect::new(r.x + 8, y + 2, r.w - 16, REPO_H - 4);
        let hot = self.gh_hv(repo) || g.menu.is_some_and(|(k, _)| k == MenuKind::Repo);
        if hot {
            cv.fill_round(repo, 8, strong.with_alpha(0x14));
        }
        self.tx.icon(cv, "ui/github", repo.x + 8, repo.y + (repo.h - 14) / 2, 14, dim, repo);
        if let Some(info) = g.ready().cloned() {
            let name = info.slug.rsplit('/').next().unwrap_or(&info.slug).to_string();
            let owner = info.slug.split('/').next().unwrap_or("").to_string();
            let nw = self.tx.ui_width(&name, true);
            let label_x = repo.x + 28;
            let room = repo.w - 28 - 24;
            let shown = fit(&mut self.tx, &name, room, true);
            self.tx.draw_ui(cv, label_x, repo.y + Self::gh_ty(repo.h), &shown, strong, repo, true);
            if nw + 8 < room {
                let sub = fit(&mut self.tx, &format!("  {owner}"), room - nw, false);
                self.tx.draw_ui(cv, label_x + nw, repo.y + Self::gh_ty(repo.h), &sub, dim, repo, false);
            }
        }
        self.tx.icon(cv, "ui/chevron_down", repo.right() - 20, repo.y + (repo.h - 12) / 2, 12, dim, repo);
        g.hits.push((repo, GhHit::RepoMenu));
        y += REPO_H;

        // Search, filter, new.
        let has_new = matches!(g.section, Section::Prs | Section::Issues);
        let has_filter = g.section != Section::Inbox;
        let mut right = r.right() - 8;
        if has_new {
            let b = Rect::new(right - 28, y + 5, 28, 28);
            self.gh_icon_btn(cv, g, b, "ui/plus", GhHit::New, false);
            right -= 30;
        }
        if has_filter {
            let b = Rect::new(right - 28, y + 5, 28, 28);
            let on = g.menu.is_some_and(|(k, _)| k == MenuKind::Filter) || !self.gh_filter_is_default(g);
            self.gh_icon_btn(cv, g, b, "ui/filter", GhHit::FilterMenu, on);
            right -= 30;
        }
        let f = Rect::new(r.x + 10, y + 5, (right - 4 - (r.x + 10)).max(40), 28);
        self.gh_draw_search(cv, g, f);
        y += TOOL_H;
        cv.fill(Rect::new(r.x, y - 1, r.w, 1), c.border);

        let body = Rect::new(r.x, y, r.w, (r.bottom() - y).max(0));
        self.gh_draw_rows(cv, g, body);
    }

    fn gh_filter_is_default(&self, g: &GhPanel) -> bool {
        match g.section {
            Section::Prs => g.pr_filter == ghub::PrFilter::default(),
            Section::Issues => g.issue_filter == ghub::IssueFilter::default(),
            Section::Actions => g.run_filter == RunFilter::default(),
            Section::Inbox => true,
        }
    }

    fn gh_draw_search(&mut self, cv: &mut Canvas, g: &mut GhPanel, f: Rect) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let on = g.query_focus && g.focused;
        cv.card(f, 7, c.widget_bg, if on { c.accent } else { c.border });
        g.hits.push((f, GhHit::Search));
        self.tx.icon(cv, "hugeicons/search", f.x + 8, f.y + (f.h - 14) / 2, 14, dim, f);
        let has = !g.query.text.is_empty();
        let tx = f.x + 29;
        let right = f.right() - if has { 26 } else { 8 };
        let clip = Rect::new(f.x, f.y, (right - f.x).max(0), f.h);
        let ph = format!("Filter {}…", g.section.noun());
        let (txt, col) = if has { (g.query.text.clone(), self.theme.foreground) } else { (ph, dim) };
        if g.query.sel_all && has {
            let w = self.tx.ui_width(&txt, false);
            cv.fill_round_clip(Rect::new(tx - 2, f.y + 3, w + 4, f.h - 6), 4, c.accent.with_alpha(0x55), clip);
        }
        self.tx.draw_ui(cv, tx, f.y + Self::gh_ty(f.h), &txt, col, clip, false);
        if on {
            let prefix: String = g.query.text.chars().take(g.query.cursor).collect();
            let cx = tx + self.tx.ui_width(&prefix, false);
            cv.fill_clip(Rect::new(cx, f.y + (f.h - 16) / 2, 1, 16), self.theme.foreground, clip);
        }
        if has {
            let b = Rect::new(f.right() - 24, f.y + (f.h - 20) / 2, 20, 20);
            if self.gh_hv(b) {
                cv.fill_round(b, 5, c.tab_active_fg.with_alpha(0x18));
            }
            self.tx.icon(cv, "ui/close", b.x + 4, b.y + 4, 12, dim, b);
            g.hits.push((b, GhHit::SearchClear));
        }
    }

    /// The rows of the list for the section: group headers and items.
    pub(super) fn gh_list_rows(&self, g: &mut GhPanel) -> Vec<LRow> {
        let groups = g.groups();
        let force = !g.query_text().is_empty();
        let mut rows = Vec::new();
        g.group_keys.clear();
        for gr in &groups {
            let key = format!("{:?}:{}", g.section, gr.id);
            let idx = g.group_keys.len();
            let open = force || !g.collapsed.contains(&key);
            g.group_keys.push(key);
            rows.push(LRow::Group(idx));
            if open {
                rows.extend(gr.items.iter().map(|&i| LRow::Item(i)));
            }
        }
        rows
    }

    fn gh_draw_rows(&mut self, cv: &mut Canvas, g: &mut GhPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (list_err, loaded, count) = match g.section {
            Section::Prs => g.pr_list().map_or((None, false, 0), |l| (l.error.clone(), l.items.is_some(), l.len())),
            Section::Issues => {
                g.issue_list().map_or((None, false, 0), |l| (l.error.clone(), l.items.is_some(), l.len()))
            }
            Section::Actions => (g.runs.error.clone(), g.runs.items.is_some(), g.runs.len()),
            Section::Inbox => (g.inbox.error.clone(), g.inbox.items.is_some(), g.inbox.len()),
        };
        let rows = self.gh_list_rows(g);
        let message = |s: &mut Self, cv: &mut Canvas, g: &mut GhPanel, t: &str, retry: bool| {
            let lines = md::wrap(t, (body.w - 32).max(80), &mut |x| s.tx.ui_width(x, false));
            let mut y = body.y + 36;
            for l in &lines {
                let lw = s.tx.ui_width(l, false);
                s.tx.draw_ui(cv, body.x + (body.w - lw) / 2, y, l, dim, body, false);
                y += 20;
            }
            if retry {
                let b = items::Btn {
                    label: "Try again".into(),
                    icon: "ui/rotate_cw",
                    hit: GhHit::Retry,
                    style: BtnStyle::Plain,
                };
                let bw = 12 + 16 + 6 + s.tx.ui_width(&b.label, false) + 12;
                s.gh_button(cv, g, body.x + (body.w - bw) / 2, y + 10, &b, body);
            }
        };
        if let (Some(e), 0) = (&list_err, count) {
            return message(self, cv, g, e, true);
        }
        if !loaded {
            let t = format!("Loading {}…", g.section.noun());
            return message(self, cv, g, &t, false);
        }
        if rows.is_empty() {
            let t = if count == 0 {
                match g.section {
                    Section::Actions => "No workflow runs yet".to_string(),
                    s => format!("No {}", s.noun()),
                }
            } else {
                format!("No matching {}", g.section.noun())
            };
            return message(self, cv, g, &t, false);
        }
        g.list_h = body.h;
        let heights: Vec<i32> =
            rows.iter().map(|r| if matches!(r, LRow::Group(_)) { GROUP_H } else { ROW_H }).collect();
        let total: i32 = heights.iter().sum::<i32>() + 12;
        g.scroll[0] = g.scroll[0].clamp(0.0, (total - body.h).max(0) as f64);
        let mut y = body.y - g.scroll[0] as i32;
        let groups = g.groups();
        let now = now_secs();
        for (k, row) in rows.iter().enumerate() {
            let h = heights[k];
            if y + h >= body.y && y <= body.bottom() {
                match *row {
                    LRow::Group(gi) => {
                        let title = groups
                            .iter()
                            .find(|gr| format!("{:?}:{}", g.section, gr.id) == g.group_keys[gi])
                            .map(|gr| (gr.title.clone(), gr.items.len()))
                            .unwrap_or_default();
                        let gr = Rect::new(body.x + 6, y, body.w - 12, h);
                        let open = !g.collapsed.contains(&g.group_keys[gi]) || !g.query_text().is_empty();
                        let vis = gr.intersect(&body);
                        let hot = self.gh_hv(vis);
                        if hot {
                            cv.fill_round_clip(gr, 8, strong.with_alpha(0x0c), body);
                        }
                        self.tx.icon(
                            cv,
                            if open { "ui/chevron_down" } else { "ui/chevron_right" },
                            gr.x + 6,
                            y + 8,
                            12,
                            dim,
                            vis,
                        );
                        self.tx.draw_ui(cv, gr.x + 24, y + Self::gh_ty(h), &title.0, strong, vis, true);
                        let tw = self.tx.ui_width(&title.0, true);
                        self.tx.draw_ui(
                            cv,
                            gr.x + 24 + tw + 8,
                            y + Self::gh_ty(h),
                            &title.1.to_string(),
                            dim,
                            vis,
                            false,
                        );
                        g.hits.push((vis, GhHit::Group(gi)));
                    }
                    LRow::Item(i) => {
                        let rr = Rect::new(body.x + 6, y, body.w - 12, h);
                        self.gh_draw_list_item(cv, g, rr, body, i, now);
                    }
                }
            }
            y += h;
        }
        let (pos, visible) = (g.scroll[0], body.h as f64);
        self.region_bar(cv, Reg::GhList, body, total as f64, visible, pos, 1.0, false);
    }

    fn gh_draw_list_item(&mut self, cv: &mut Canvas, g: &mut GhPanel, row: Rect, body: Rect, i: usize, now: i64) {
        let c = self.theme.chrome;
        let vis = row.intersect(&body);
        let selected = g.selected_row() == Some(i);
        let hot = self.gh_hv(vis) || (g.cursor == Some(i) && g.focused && !g.query_focus);
        if selected {
            cv.fill_round_clip(row, 8, c.list_active_bg, body);
        } else if hot {
            cv.fill_round_clip(row, 8, c.list_hover_bg, body);
        }
        match g.section {
            Section::Prs => {
                let Some(p) = g.pr_list().and_then(|l| l.items.as_ref()).and_then(|v| v.get(i)).cloned() else {
                    return;
                };
                self.gh_draw_pr_row(cv, &p, row, vis, now, selected);
            }
            Section::Issues => {
                let Some(it) = g.issue_list().and_then(|l| l.items.as_ref()).and_then(|v| v.get(i)).cloned() else {
                    return;
                };
                self.gh_draw_issue_row(cv, &it, row, vis, now);
            }
            Section::Actions => {
                let Some(r) = g.runs.items.as_ref().and_then(|v| v.get(i)).cloned() else { return };
                self.gh_draw_run_row(cv, &r, row, vis, now);
            }
            Section::Inbox => {
                let Some(n) = g.inbox.items.as_ref().and_then(|v| v.get(i)).cloned() else { return };
                self.gh_draw_notif_row(cv, &n, row, vis, now);
            }
        }
        g.hits.push((vis, GhHit::Row(i)));
    }

    fn gh_row_text(&mut self, cv: &mut Canvas, row: Rect, vis: Rect, title: &str, time: &str, bold: bool) -> i32 {
        let c = self.theme.chrome;
        let tx = row.x + 42;
        let tw = if time.is_empty() { 0 } else { self.tx.ui_width(time, false) + 8 };
        let room = (row.right() - 10 - tx - tw).max(20);
        let t = fit(&mut self.tx, title, room, bold);
        self.tx.draw_ui(cv, tx, row.y + 8, &t, c.tab_active_fg, vis, bold);
        if !time.is_empty() {
            self.tx.draw_ui(cv, row.right() - 10 - tw + 8, row.y + 8, time, c.tab_inactive_fg, vis, false);
        }
        tx
    }

    fn gh_draw_pr_row(&mut self, cv: &mut Canvas, p: &PrItem, row: Rect, vis: Rect, now: i64, _selected: bool) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        self.gh_avatar(cv, row.x + 10, row.y + (row.h - 24) / 2, 24, &p.author, vis);
        let tx = self.gh_row_text(cv, row, vis, &p.title, &short_time(&p.updated, now), false);
        let mut x = tx;
        let y2 = row.y + 28;
        let sub = format!("#{} · {}", p.number, p.author);
        let sw = self.tx.ui_width(&sub, false);
        self.tx.draw_ui(cv, x, y2 + 1, &sub, dim, vis, false);
        x += sw + 8;
        let st = PrStatus::of(p);
        if st != PrStatus::Open && x + 50 < row.right() {
            x += self.gh_pill(cv, x, y2, st.label(), self.gh_color(st.tone()), vis) + 4;
        }
        if let Some((l, t)) = model::review_badge(&p.review).filter(|_| matches!(st, PrStatus::Open | PrStatus::Draft))
            && x + 80 < row.right()
        {
            x += self.gh_pill(cv, x, y2, l, self.gh_color(t), vis) + 4;
        }
        if !p.checks.is_empty() && x + 30 < row.right() {
            let t = model::tally(&p.checks);
            let (icon, tone) = if t.fail > 0 {
                ("ui/x_circle_filled", Tone::Error)
            } else if t.pending > 0 {
                ("ui/clock", Tone::Warning)
            } else {
                ("ui/check", Tone::Success)
            };
            self.tx.icon(cv, icon, x, y2 + 2, 14, self.gh_color(tone), vis);
        }
    }

    fn gh_draw_issue_row(&mut self, cv: &mut Canvas, it: &IssueItem, row: Rect, vis: Rect, now: i64) {
        let c = self.theme.chrome;
        self.gh_avatar(cv, row.x + 10, row.y + (row.h - 24) / 2, 24, &it.author, vis);
        let tx = self.gh_row_text(cv, row, vis, &it.title, &short_time(&it.updated, now), false);
        let y2 = row.y + 28;
        let sub = format!("#{} · {}", it.number, it.author);
        let sw = self.tx.ui_width(&sub, false);
        self.tx.draw_ui(cv, tx, y2 + 1, &sub, c.tab_inactive_fg, vis, false);
        let mut x = tx + sw + 10;
        for l in it.labels.iter().take(3) {
            if let Some((r, g, b)) = model::label_rgb(&l.color) {
                cv.fill_round_clip(Rect::new(x, y2 + 6, 8, 8), 4, Rgba(r, g, b, 0xff), vis);
                x += 12;
            }
        }
        if it.comments > 0 && x + 30 < row.right() {
            self.tx.icon(cv, "ui/chat", x + 2, y2 + 2, 14, c.tab_inactive_fg, vis);
            self.tx.draw_ui(cv, x + 20, y2 + 1, &it.comments.to_string(), c.tab_inactive_fg, vis, false);
        }
    }

    fn gh_draw_run_row(&mut self, cv: &mut Canvas, r: &RunItem, row: Rect, vis: Rect, now: i64) {
        let c = self.theme.chrome;
        let s = model::run_state(&r.status, &r.conclusion);
        let col = self.gh_color(s.tone);
        let badge = Rect::new(row.x + 10, row.y + (row.h - 24) / 2, 24, 24);
        cv.fill_round_clip(badge, 12, col.with_alpha(0x26), vis);
        self.tx.icon(cv, phase_icon(s.phase), badge.x + 5, badge.y + 5, 14, col, vis);
        let when = if r.created.is_empty() { &r.updated } else { &r.created };
        let tx = self.gh_row_text(cv, row, vis, &r.title, &short_time(when, now), false);
        let mut sub = format!("#{} · {}", r.number, r.branch);
        if !r.event.is_empty() {
            sub.push_str(&format!(" · {}", r.event));
        }
        if let Some(d) =
            model::elapsed(if r.started.is_empty() { &r.created } else { &r.started }, &r.updated, s.active, now)
        {
            sub.push_str(&format!(" · {d}"));
        }
        let sub = fit(&mut self.tx, &sub, (row.right() - 10 - tx).max(20), false);
        self.tx.draw_ui(cv, tx, row.y + 29, &sub, c.tab_inactive_fg, vis, false);
    }

    fn gh_draw_notif_row(&mut self, cv: &mut Canvas, n: &Notif, row: Rect, vis: Rect, now: i64) {
        let c = self.theme.chrome;
        let icon = match n.kind.as_str() {
            "PullRequest" => "ui/pull_request",
            "Issue" => "ui/info",
            "CheckSuite" => "ui/bolt_filled",
            _ => "ui/bell",
        };
        let col = if n.unread { c.accent_text } else { c.tab_inactive_fg };
        let badge = Rect::new(row.x + 10, row.y + (row.h - 24) / 2, 24, 24);
        cv.fill_round_clip(badge, 12, col.with_alpha(0x22), vis);
        self.tx.icon(cv, icon, badge.x + 5, badge.y + 5, 14, col, vis);
        let tx = self.gh_row_text(cv, row, vis, &n.title, &short_time(&n.updated, now), n.unread);
        let sub = format!("{} · {}", n.repo, n.reason.replace('_', " "));
        let sub = fit(&mut self.tx, &sub, (row.right() - 10 - tx - 14).max(20), false);
        self.tx.draw_ui(cv, tx, row.y + 29, &sub, c.tab_inactive_fg, vis, false);
        if n.unread {
            cv.fill_round_clip(Rect::new(row.right() - 18, row.y + 33, 8, 8), 4, c.accent, vis);
        }
    }

    // ───────────── dropdowns ─────────────

    pub fn gh_menu_items(&self, g: &GhPanel, kind: MenuKind) -> Vec<MItem> {
        let item = |label: &str, icon: &'static str, hit: GhHit, checked: Option<bool>| MItem {
            label: label.into(),
            icon,
            hit,
            checked,
        };
        match kind {
            MenuKind::Repo => {
                let Some(url) = g.web_url() else { return Vec::new() };
                vec![
                    item("Open on GitHub", "ui/arrow_up_right", GhHit::Link(url.clone()), None),
                    item("Pull requests on GitHub", "ui/pull_request", GhHit::Link(format!("{url}/pulls")), None),
                    item("Actions on GitHub", "ui/bolt_filled", GhHit::Link(format!("{url}/actions")), None),
                    item("Copy repository URL", "ui/copy", GhHit::Link(format!("copy:{url}")), None),
                ]
            }
            MenuKind::Filter => match g.section {
                Section::Prs => ghub::PrFilter::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, f)| item(f.label(), "ui/filter", GhHit::MenuItem(i), Some(*f == g.pr_filter)))
                    .collect(),
                Section::Issues => ghub::IssueFilter::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, f)| item(f.label(), "ui/filter", GhHit::MenuItem(i), Some(*f == g.issue_filter)))
                    .collect(),
                Section::Actions => RunFilter::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, f)| item(f.label(), "ui/filter", GhHit::MenuItem(i), Some(*f == g.run_filter)))
                    .collect(),
                Section::Inbox => Vec::new(),
            },
            MenuKind::Merge => vec![
                item("Create a merge commit", "ui/git_merge_conflict", GhHit::Ask(state::Act::Merge("merge")), None),
                item("Squash and merge", "ui/git_merge_conflict", GhHit::Ask(state::Act::Merge("squash")), None),
                item("Rebase and merge", "ui/git_merge_conflict", GhHit::Ask(state::Act::Merge("rebase")), None),
            ],
        }
    }

    fn gh_draw_menu(&mut self, cv: &mut Canvas, g: &mut GhPanel, p: Rect) {
        let Some((kind, anchor)) = g.menu else { return };
        let items = self.gh_menu_items(g, kind);
        if items.is_empty() {
            g.menu = None;
            return;
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        const ROW: i32 = 28;
        const PAD: i32 = 4;
        let mut w = 150;
        for it in &items {
            w = w.max(10 + 16 + 8 + self.tx.ui_width(&it.label, false) + 14);
        }
        let w = w.min((p.w - 16).max(150));
        let h = items.len() as i32 * ROW + 2 * PAD;
        let x = if kind == MenuKind::Repo || kind == MenuKind::Merge { anchor.x } else { anchor.right() - w };
        let x = x.clamp(p.x + 8, (p.right() - 8 - w).max(p.x + 8));
        let below = anchor.bottom() + 4;
        let y = if below + h <= p.bottom() - 4 { below } else { (anchor.y - 4 - h).max(p.y + 4) };
        let r = Rect::new(x, y, w, h);
        cv.shadow(r, 10);
        cv.fill_round(r, 10, c.widget_bg);
        cv.ring(r, 10, c.border);
        g.hits.push((r, GhHit::MenuPad));
        for (i, it) in items.iter().enumerate() {
            let ir = Rect::new(r.x + 4, r.y + PAD + i as i32 * ROW, r.w - 8, ROW);
            let hot = self.gh_hv(ir);
            if hot {
                cv.fill_round(ir, 6, c.list_hover_bg);
            }
            match it.checked {
                Some(true) => self.tx.icon(cv, "ui/check", ir.x + 6, ir.y + 6, 16, c.accent_text, ir),
                Some(false) => {}
                None => self.tx.icon(cv, it.icon, ir.x + 6, ir.y + 6, 16, if hot { strong } else { dim }, ir),
            }
            self.tx.draw_ui(cv, ir.x + 30, ir.y + Self::gh_ty(ROW), &it.label, strong, ir, false);
            g.hits.push((ir, GhHit::MenuItem(i)));
        }
    }
}
