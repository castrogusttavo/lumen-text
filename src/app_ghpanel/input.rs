//! Opening and closing the page, clicks, keys, the cursor and the background pump.

use super::*;
use crate::app::Focus;
use items::{act_label, confirm_for};
use state::{Act, Detail, Env, GhHit, GhPanel, Load, MenuKind, PrTab, Section};

impl App {
    // ───────────── showing the page ─────────────

    /// Show the page (the Git page and the AI panel step aside), with the keyboard on it.
    pub fn gh_show(&mut self) {
        if self.gitpanel.is_some() {
            self.close_git_panel();
        }
        self.ai_hide();
        let root = self.git_root();
        let wake = self.wake();
        let g = self.gh.get_or_insert_with(|| Box::new(GhPanel::new(&root)));
        // Another project since the page was last open: start over (the cache is the old one's).
        if g.root != root {
            **g = GhPanel::new(&root);
        }
        g.wake = Some(wake);
        g.open = true;
        g.focused = true;
        match g.env.clone() {
            Env::Ready(_) => g.load_section(false),
            Env::Checking if g.env_sent => {}
            // `gh` may have been installed or logged in since: look again.
            _ => g.check_env(),
        }
        self.focus = Focus::Editor;
    }

    pub fn gh_hide(&mut self) {
        if let Some(g) = self.gh.as_mut()
            && g.open
        {
            g.open = false;
            g.focused = false;
            g.menu = None;
            g.query_focus = false;
            g.composer_focus = false;
            self.focus = Focus::Editor;
        }
    }

    /// Ctrl+Alt+G: open, or focus, or close when it already has the focus.
    pub fn gh_toggle(&mut self) {
        match self.gh.as_mut() {
            Some(g) if g.open && g.focused => self.gh_hide(),
            Some(g) if g.open => g.focused = true,
            _ => self.gh_show(),
        }
    }

    /// The rail's GitHub button: plain open / close.
    pub fn gh_rail(&mut self) {
        if self.gh_open() { self.gh_hide() } else { self.gh_show() }
    }

    pub fn gh_open_section(&mut self, s: Section) {
        self.gh_show();
        if let Some(g) = self.gh.as_mut() {
            g.set_section(s);
        }
    }

    /// "GitHub: Refresh": the list and the open detail again.
    pub fn gh_refresh_all(&mut self) {
        self.gh_show();
        if let Some(g) = self.gh.as_mut() {
            g.refresh();
        }
    }

    // ───────────── the pump ─────────────

    /// Per-event hook: apply what the workers sent, keep lists fresh while the page is open.
    pub fn gh_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let root = self.git_root();
        let Some(mut g) = self.gh.take() else { return };
        // The project changed under a hidden page: forget everything (the next show reloads).
        if g.root != root {
            if !g.open {
                self.gh = None;
                return;
            }
            // Open while the project changed: the lists belong to the old repository.
            let (wake, focused) = (g.wake.clone(), g.focused);
            *g = GhPanel::new(&root);
            g.wake = wake;
            g.open = true;
            g.focused = focused;
            g.check_env();
        }
        let (changed, done) = g.pump();
        if changed {
            self.request_redraw();
        }
        let mut git_moved = false;
        for (label, result, act) in done {
            if matches!(act, Act::Checkout) && result.is_ok() {
                git_moved = true;
            }
            if let Err(e) = &result
                && act != Act::Checkout
            {
                self.toast(format!("GitHub: {e}"), false);
            }
            g.after_act(&label, &result, &act);
            if let (Ok(_), Act::Checkout) = (&result, &act) {
                self.toast("Branch checked out", true);
            }
        }
        if g.open && g.ready().is_some() {
            // The visible list is asked again once its cache is old, a running run every few seconds.
            if !g.loading_now() && g.needs_refresh() {
                g.load_section(true);
            }
            if g.run_active()
                && let Detail::Run(v) = &mut g.detail
                && v.polled.elapsed() >= RUN_POLL
            {
                v.polled = Instant::now();
                let id = v.id;
                g.fetch_run(id);
            }
            want(now + Duration::from_secs(2));
            if g.busy.is_some() || g.loading_now() {
                self.request_redraw();
            }
        }
        self.gh = Some(g);
        if git_moved {
            self.refresh_git_status();
            self.sync_docs_with_head();
        }
    }

    // ───────────── pointer ─────────────

    /// Mouse press: true when the page took it.
    pub fn ghpanel_mouse(&mut self, x: f64, y: f64, button: u8) -> bool {
        if !self.gh_open() {
            return false;
        }
        let Some(mut g) = self.gh.take() else { return false };
        let r = self.gh_rect();
        if !r.contains(x, y) {
            g.focused = false;
            g.query_focus = false;
            g.composer_focus = false;
            if let Detail::Run(v) = &mut g.detail {
                v.log_filter_focus = false;
            }
            g.menu = None;
            self.gh = Some(g);
            return false;
        }
        g.focused = true;
        self.focus = Focus::Editor;
        let mut hit = g.hit_at(x, y);
        // Any click closes the open dropdown; an item of it acts like the same button elsewhere.
        let open_menu = g.menu.take();
        if let Some(GhHit::MenuItem(n)) = hit.clone() {
            hit = match open_menu {
                Some((MenuKind::Filter, _)) => {
                    g.apply_filter(n);
                    None
                }
                Some((k, _)) => self.gh_menu_items(&g, k).get(n).map(|i| i.hit.clone()),
                None => None,
            };
        } else if matches!(hit, Some(GhHit::MenuPad)) {
            hit = None;
        }
        // The same button again closes its menu.
        let kind = match &hit {
            Some(GhHit::RepoMenu) => Some(MenuKind::Repo),
            Some(GhHit::FilterMenu) => Some(MenuKind::Filter),
            Some(GhHit::MergeMenu) => Some(MenuKind::Merge),
            _ => None,
        };
        let anchor = hit.as_ref().and_then(|h| g.hits.iter().find(|(_, x)| x == h).map(|(r, _)| *r));
        if let (Some(k), Some(a)) = (kind, anchor) {
            if open_menu.map(|(m, _)| m) != Some(k) {
                g.menu = Some((k, a));
            }
        } else {
            g.query_focus = matches!(hit, Some(GhHit::Search));
            g.composer_focus = matches!(hit, Some(GhHit::Composer));
            if let Detail::Run(v) = &mut g.detail {
                v.log_filter_focus = matches!(hit, Some(GhHit::LogFilter));
            }
            // Clicking elsewhere drops a pending question.
            if g.confirm.is_some() && !matches!(hit, Some(GhHit::Yes | GhHit::No | GhHit::Noop | GhHit::Ask(_))) {
                g.confirm = None;
            }
            if let Some(h) = hit {
                self.gh_do(&mut g, h, x, y, button);
            }
        }
        self.gh = Some(g);
        true
    }

    fn gh_do(&mut self, g: &mut GhPanel, hit: GhHit, x: f64, y: f64, button: u8) {
        match hit {
            GhHit::Section(s) => g.set_section(s),
            GhHit::Refresh => g.refresh(),
            GhHit::Retry => {
                if g.ready().is_none() {
                    g.check_env();
                } else {
                    g.refresh();
                }
            }
            GhHit::RepoMenu | GhHit::FilterMenu | GhHit::MergeMenu => {}
            GhHit::New => {
                if let Some(u) = g.web_url() {
                    let path = if g.section == Section::Issues { "issues/new" } else { "compare" };
                    crate::paths::open_url(&format!("{u}/{path}"));
                }
            }
            GhHit::Search | GhHit::LogFilter | GhHit::Noop | GhHit::MenuItem(_) | GhHit::MenuPad => {}
            GhHit::SearchClear => {
                g.query = Default::default();
                g.query_focus = true;
                g.scroll[0] = 0.0;
            }
            GhHit::Group(i) => {
                if let Some(k) = g.group_keys.get(i).cloned()
                    && !g.collapsed.remove(&k)
                {
                    g.collapsed.insert(k);
                }
            }
            GhHit::Row(i) => self.gh_open_row(g, i, button),
            GhHit::Back => {
                g.detail = Detail::None;
                g.reset_detail_view();
            }
            GhHit::OpenWeb => {
                if let Some(u) = g.detail_url() {
                    crate::paths::open_url(&u);
                }
            }
            GhHit::CopyLink => {
                if let Some(u) = g.detail_url() {
                    self.clip.set(&u);
                    self.toast("Link copied", true);
                }
            }
            GhHit::Tab(t) => {
                if let Detail::Pr(v) = &mut g.detail {
                    v.tab = t;
                }
                g.scroll[1] = 0.0;
                if t == PrTab::Files {
                    g.load_pr_diff();
                }
            }
            GhHit::File(i) => {
                if let Detail::Pr(v) = &mut g.detail {
                    v.file = if v.file == Some(i) { None } else { Some(i) };
                }
                g.load_pr_diff();
            }
            GhHit::Commit(i) => {
                let sha = match &g.detail {
                    Detail::Pr(v) => v.detail.ready().and_then(|d| d.commits.get(i)).map(|c| c.oid.clone()),
                    _ => None,
                };
                if let Some(s) = sha {
                    self.clip.set(&s);
                    self.toast("Commit SHA copied", true);
                }
            }
            GhHit::Check(i) => {
                let url = match &g.detail {
                    Detail::Pr(v) => v.detail.ready().and_then(|d| d.item.checks.get(i)).map(|c| c.url.clone()),
                    _ => None,
                };
                match url {
                    Some(u) if !u.is_empty() => crate::paths::open_url(&u),
                    _ => self.toast("This check has no link", false),
                }
            }
            GhHit::Job(i) => g.open_log(i),
            GhHit::JobFold(i) => {
                if let Detail::Run(v) = &mut g.detail
                    && !v.folded.remove(&i)
                {
                    v.folded.insert(i);
                }
            }
            GhHit::Ask(act) => self.gh_ask(g, act),
            GhHit::Yes => {
                if let Some(c) = g.confirm.take() {
                    g.run_act(c.act.clone(), act_label(&c.act));
                }
            }
            GhHit::No => g.confirm = None,
            GhHit::Composer => {
                let tx = &mut self.tx;
                g.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
                let b = g.composer_box;
                g.composer.press(
                    (x - (b.x + 10) as f64) as f32,
                    (y - (b.y + 8) as f64) as f32,
                    1,
                    self.mods.shift_key(),
                );
            }
            GhHit::LogBack => {
                if let Detail::Run(v) = &mut g.detail {
                    v.job = None;
                    v.log = Load::Idle;
                }
            }
            GhHit::LogFailed => {
                let job = if let Detail::Run(v) = &mut g.detail {
                    v.log_failed_only = !v.log_failed_only;
                    v.job
                } else {
                    None
                };
                if let Some(j) = job {
                    g.open_log(j);
                }
            }
            GhHit::LogFilterClear => {
                if let Detail::Run(v) = &mut g.detail {
                    v.log_filter = Default::default();
                    v.log_filter_focus = true;
                }
            }
            GhHit::LogCopy => {
                let text = g.log_text();
                if !text.is_empty() {
                    self.clip.set(&text);
                    self.toast("Log copied", true);
                }
            }
            GhHit::Link(u) => match u.strip_prefix("copy:") {
                Some(t) => {
                    self.clip.set(t);
                    self.toast("Copied", true);
                }
                None => crate::paths::open_url(&u),
            },
        }
    }

    /// A change was asked for: some run at once, the ones that cannot be undone ask first.
    fn gh_ask(&mut self, g: &mut GhPanel, act: Act) {
        if g.busy.is_some() {
            return;
        }
        let needs_text = matches!(act, Act::CommentPr | Act::CommentIssue | Act::RequestChanges);
        if needs_text && g.composer.text().trim().is_empty() {
            g.composer_focus = true;
            self.toast("Write something in the box first", false);
            return;
        }
        match confirm_for(&act, g.detail_number()) {
            Some(c) => g.confirm = Some(c),
            None => g.run_act(act.clone(), act_label(&act)),
        }
    }

    /// A click on a row of the list.
    fn gh_open_row(&mut self, g: &mut GhPanel, i: usize, _button: u8) {
        g.cursor = Some(i);
        if g.section != Section::Inbox {
            g.open_row(i);
            return;
        }
        let Some(n) = g.inbox.items.as_ref().and_then(|v| v.get(i)).cloned() else { return };
        g.mark_read_local(i);
        if n.unread {
            g.spawn_mark_read(&n.id);
        }
        let mine = g.ready().is_some_and(|r| r.slug == n.repo);
        match (mine, n.kind.as_str(), n.number) {
            (true, "PullRequest", Some(num)) => {
                g.set_section(Section::Prs);
                g.open_pr(num);
            }
            (true, "Issue", Some(num)) => {
                g.set_section(Section::Issues);
                g.open_issue(num);
            }
            (_, kind, num) => {
                let base = format!("https://github.com/{}", n.repo);
                let url = match (kind, num) {
                    ("PullRequest", Some(num)) => format!("{base}/pull/{num}"),
                    ("Issue", Some(num)) => format!("{base}/issues/{num}"),
                    ("CheckSuite", _) => format!("{base}/actions"),
                    _ => base,
                };
                crate::paths::open_url(&url);
            }
        }
    }

    pub fn ghpanel_release(&mut self) {
        let selected = self.gh.as_mut().is_some_and(|g| g.open && g.composer.release());
        if selected {
            self.request_redraw();
        }
    }

    /// Pointer moved: a selection drag in the comment box; true when the page needs a redraw.
    pub fn ghpanel_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let page = self.gh_rect();
        let Some(g) = self.gh.as_mut().filter(|g| g.open) else { return false };
        if g.composer.dragging() {
            let b = g.composer_box;
            let tx = &mut self.tx;
            g.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
            g.composer.drag_to((x - (b.x + 10) as f64) as f32, (y - (b.y + 8) as f64) as f32);
            return true;
        }
        // Hover tints follow the pointer: a redraw only when what is under it changed.
        let inside = page.contains(x, y);
        let h = if inside { g.hit_at(x, y) } else { None };
        let changed = h != g.hover;
        g.hover = h;
        changed
    }

    /// Pointer: a hand over what clicks, an I-beam over the text boxes.
    pub fn ghpanel_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let g = self.gh.as_ref().filter(|g| g.open)?;
        if !self.gh_rect().contains(x, y) {
            return None;
        }
        Some(match g.hit_at(x, y) {
            Some(GhHit::Search | GhHit::Composer | GhHit::LogFilter) => CursorIcon::Text,
            Some(GhHit::Noop | GhHit::MenuPad) | None => CursorIcon::Default,
            Some(_) => CursorIcon::Pointer,
        })
    }

    // ───────────── keys ─────────────

    /// Keys while the page has the focus; true when consumed.
    pub fn ghpanel_key(&mut self, ev: &KeyIn) -> bool {
        if !self.gh_open() {
            return false;
        }
        let Some(mut g) = self.gh.take() else { return false };
        let consumed = g.focused && self.gh_key(&mut g, ev);
        self.gh = Some(g);
        consumed
    }

    /// A key typed into a single-line field. False: it is a shortcut for the app.
    fn gh_field_key(&mut self, f: &mut crate::field::TextField, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        match f.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => return true,
            KeyAct::Copy(t) => {
                self.clip.set(&t);
                return true;
            }
            KeyAct::Paste => {
                let t = self.clip.get();
                f.insert(&t.replace(['\n', '\r'], " "));
                return true;
            }
            KeyAct::Ignored => {}
        }
        match ev.text.as_deref() {
            Some(t) if !ctrl && !alt && !t.is_empty() && !t.chars().any(char::is_control) => {
                f.insert(t);
                true
            }
            _ => false,
        }
    }

    fn gh_key(&mut self, g: &mut GhPanel, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let key = &ev.key;
        let esc = matches!(key, Key::Named(NamedKey::Escape));
        let enter = matches!(key, Key::Named(NamedKey::Enter));
        if esc && g.menu.take().is_some() {
            return true;
        }
        // The comment box.
        if g.composer_focus {
            if esc || (matches!(key, Key::Named(NamedKey::Tab)) && !ctrl) {
                g.composer_focus = false;
                return true;
            }
            if enter && ctrl {
                let act = match &g.detail {
                    Detail::Issue(_) => Act::CommentIssue,
                    _ => Act::CommentPr,
                };
                self.gh_ask(g, act);
                return true;
            }
            let tx = &mut self.tx;
            g.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
            match g.composer.key(key, ctrl, shift, alt) {
                KeyAct::Handled => return true,
                KeyAct::Copy(t) => {
                    self.clip.set(&t);
                    return true;
                }
                KeyAct::Paste => {
                    let t = self.clip.get();
                    g.composer.insert(&t);
                    return true;
                }
                KeyAct::Ignored => {}
            }
            let text = ev.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
            if (ctrl || alt) && !(ctrl && alt && text.is_some()) {
                return false;
            }
            if let Some(t) = text {
                g.composer.type_text(t);
            }
            return true;
        }
        // The list filter.
        if g.query_focus {
            if esc {
                if g.query.text.is_empty() {
                    g.query_focus = false;
                } else {
                    g.query = Default::default();
                }
                return true;
            }
            if enter {
                g.query_focus = false;
                if let Some(&i) = g.shown_order().first() {
                    self.gh_open_row(g, i, 0);
                }
                return true;
            }
            let mut q = std::mem::take(&mut g.query);
            let used = self.gh_field_key(&mut q, ev);
            g.query = q;
            g.scroll[0] = 0.0;
            return used;
        }
        // The log filter.
        if let Detail::Run(v) = &mut g.detail
            && v.log_filter_focus
        {
            if esc {
                if v.log_filter.text.is_empty() {
                    v.log_filter_focus = false;
                } else {
                    v.log_filter = Default::default();
                }
                return true;
            }
            if enter {
                v.log_filter_focus = false;
                return true;
            }
            let mut f = std::mem::take(&mut v.log_filter);
            let used = self.gh_field_key(&mut f, ev);
            if let Detail::Run(v) = &mut g.detail {
                v.log_filter = f;
            }
            g.scroll[2] = 0.0;
            return used;
        }
        if ctrl || alt {
            return false;
        }
        // Navigation.
        if esc {
            if g.confirm.take().is_some() {
                return true;
            }
            if let Detail::Run(v) = &mut g.detail
                && v.job.is_some()
            {
                v.job = None;
                v.log = Load::Idle;
                return true;
            }
            if !matches!(g.detail, Detail::None) {
                g.detail = Detail::None;
                g.reset_detail_view();
                return true;
            }
            self.gh_hide();
            return true;
        }
        match key {
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                let order = g.shown_order();
                if order.is_empty() {
                    return true;
                }
                let down = matches!(key, Key::Named(NamedKey::ArrowDown));
                let at = g.cursor.or_else(|| g.selected_row()).and_then(|c| order.iter().position(|&i| i == c));
                let next = match (at, down) {
                    (None, _) => 0,
                    (Some(p), true) => (p + 1).min(order.len() - 1),
                    (Some(p), false) => p.saturating_sub(1),
                };
                g.cursor = Some(order[next]);
                g.reveal_cursor();
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(i) = g.cursor {
                    self.gh_open_row(g, i, 0);
                }
                true
            }
            Key::Character(c) => match c.as_str() {
                "/" => {
                    g.query_focus = true;
                    true
                }
                "r" => {
                    g.refresh();
                    true
                }
                "1" | "2" | "3" | "4" => {
                    let i = c.parse::<usize>().unwrap_or(1) - 1;
                    g.set_section(Section::ALL[i.min(3)]);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }
}
