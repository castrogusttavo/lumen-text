use super::*;
use crate::{
    anim,
    ds::{self, Elev},
    ui_kit::keycap::KEY_H,
    ui_modal::{self, FOOT_H},
};

/// Hints under every picker.
const PICK_HINTS: [(&str, &str); 3] = [("↑ ↓", "navigate"), ("Enter", "open"), ("Esc", "close")];

/// Hints under the local history timeline: Enter opens the comparison, which has Restore.
const HIST_HINTS: [(&str, &str); 3] = [("↑ ↓", "navigate"), ("Enter", "compare / restore"), ("Esc", "close")];

/// What an empty list tells the person: (title, hint).
fn empty_copy(kind: PickKind, query: &str) -> (&'static str, &'static str) {
    match kind {
        PickKind::ProjectSearch if query.is_empty() => {
            ("Search the project", "Type to search every file. Alt+C case, Alt+W whole word, Alt+R regex")
        }
        PickKind::ProjectSearch => ("No results", "Try fewer letters, or loosen case, word and regex"),
        PickKind::QuickOpen => {
            ("No results", "Files by name. Start with @ for symbols, # for project symbols, : for a line")
        }
        PickKind::Commands => ("No results", "Commands read like “File: Save”. Try the area first, then the action"),
        _ => ("No results", "Try fewer or different letters"),
    }
}

impl App {
    // ───────────── drawing ─────────────

    pub fn draw_find(&mut self, cv: &mut Canvas) {
        self.refresh_find();
        let Some(f) = &self.find else { return };
        let c = self.theme.chrome;
        let g = super::find::find_geo(self.layout.editor, f.show_replace);
        let r = g.card;
        cv.shadow(r, 10);
        cv.card(r, 10, c.widget_bg, c.border);
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let (mx, my) = self.mouse;
        let free = self.overlay.is_none();
        let field = |cv: &mut Canvas,
                     tx: &mut crate::ui::TextCtx,
                     b: Rect,
                     text: &str,
                     hint: &str,
                     cursor: usize,
                     focus: bool,
                     bad: bool,
                     selall: bool| {
            cv.card(
                b,
                7,
                c.input_bg,
                if focus {
                    c.accent
                } else if bad {
                    c.error
                } else {
                    c.border
                },
            );
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
            g.field,
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
                    "No results".to_string()
                }
            }
            (n, Some(c)) => format!("{} of {}", c + 1, n),
            (n, None) => format!("{n}"),
        };
        // Toggles and buttons: ghost look, a tinted pill when on, hover feedback.
        let hot = |b: Rect| free && b.contains(mx, my);
        let back = |cv: &mut Canvas, b: Rect, on: bool| {
            if on {
                cv.fill_round(b.inset(2), 6, c.accent.with_alpha(0x30));
            }
            if hot(b) {
                cv.fill_round(b.inset(2), 6, c.list_hover_bg.with_alpha(0xc0));
            }
        };
        back(cv, g.case, f.case);
        back(cv, g.regex, f.regex);
        for b in [g.prev, g.next, g.close] {
            back(cv, b, false);
        }
        let toggle = |on: bool| if on { c.accent_text } else { dim };
        let ty = g.case.y + 5;
        self.tx.draw_ui(cv, g.case.x + 5, ty, "Aa", toggle(f.case), g.case, true);
        self.tx.draw_ui(cv, g.regex.x + 6, ty, ".*", toggle(f.regex), g.regex, true);
        let cw = self.tx.ui_width(&count, false);
        let cx = g.count.x + (g.count.w - cw).max(0) / 2;
        self.tx.draw_ui(cv, cx, ty, &count, if f.valid { dim } else { c.error }, g.count, false);
        for (b, icon) in [(g.prev, "ui/chevron_up"), (g.next, "ui/chevron_down"), (g.close, "ui/close")] {
            let col = if hot(b) { c.tab_active_fg } else { dim };
            self.tx.icon(cv, icon, b.x + (b.w - 14) / 2, b.y + (b.h - 14) / 2, 14, col, b);
        }
        if f.show_replace {
            field(
                cv,
                &mut self.tx,
                g.rfield,
                &f.replace.text,
                "Replace",
                f.replace.cursor,
                f.focused && f.in_replace,
                false,
                f.replace.sel_all,
            );
            for (b, label) in [(g.replace, "Replace"), (g.all, "All")] {
                let hb = b.inset(2);
                cv.card(hb, 6, if hot(b) { c.list_hover_bg } else { c.widget_bg }, c.border);
                let lw = self.tx.ui_width(label, false);
                self.tx.draw_ui(cv, b.x + (b.w - lw) / 2, b.y + 5, label, fg, b, false);
            }
        }
    }

    pub fn draw_overlay(&mut self, cv: &mut Canvas) {
        self.layout.overlay_rows.clear();
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let base = self.picker_rect();
        self.regions.modal_from = self.regions.list.len();
        self.draw_backdrop(cv);
        let reveal = self.reveal_begin(cv);
        // (list area, total rows, scroll row) of the picker: its scrollbar is added after the borrow ends.
        let mut picker_bar: Option<(Rect, f64, f64)> = None;
        // Problems panel: the scope switch ("Open files" | "Whole project") in the input row.
        let chips = matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickKind::Problems)
            .then(|| self.problems_chips(base, INPUT_H));
        let project = self.problems_project();
        // Explorer prompts (rename, new file / folder) draw as a field in the tree when it shows.
        let inline = self.inline_field_rect();

        match &self.overlay {
            Overlay::None => {}
            Overlay::Settings(_) => self.draw_settings(cv),
            Overlay::TopMenu(_) => self.draw_topmenu(cv),
            Overlay::Ctx(_) => self.draw_ctxmenu(cv),
            Overlay::Font(_) => self.draw_fontpicker(cv),
            Overlay::Prompt(p) if inline.is_some() => {
                let (field, text_x) = inline.unwrap_or_default();
                let k = self.modal.progress();
                let f = Rect::new(field.x + p.shake_dx(), field.y, field.w, field.h);
                let ring = if p.error.is_some() { c.error } else { c.focus_ring };
                cv.shadow(f, ds::R_ROW);
                cv.card(f, ds::R_ROW, c.input_bg, anim::fade(ring, k));
                cv.ring(f.inset(-1), ds::R_ROW + 1, anim::fade(ring, 0.45 * k));
                // The entry's own icon: the folder, or the file type of the name (renaming keeps the old one).
                let icon = match (&p.kind, p.input_icon()) {
                    (_, Some(i)) => i.to_string(),
                    (PromptKind::RenamePath { from }, None) if !from.is_dir() => {
                        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        self.tx.icons.file_icon(&name)
                    }
                    _ => "file/folder".to_string(),
                };
                self.tx.icon(cv, &icon, text_x - 24, f.y + (f.h - 16) / 2, 16, dim, f);
                let ty = f.y + (f.h - 18) / 2;
                let text_x = text_x + p.shake_dx();
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(Rect::new(text_x - 2, f.y + 4, w + 4, f.h - 8), 4, c.selection_accent);
                }
                self.tx.draw_ui(cv, text_x, ty, &p.field.text, fg, f.inset(1), false);
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = text_x + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, ty, 1, 18), fg, f);
                if f.w > 220 && p.field.text.is_empty() {
                    crate::ui_kit::keycap::draw_keys_in(
                        cv,
                        &mut self.tx,
                        &c,
                        f.right() - 48,
                        f.y + (f.h - KEY_H) / 2,
                        &["Esc".to_string()],
                        k,
                        f,
                    );
                }
                if let Some(why) = &p.error {
                    let w = self.tx.ui_width(why, false) + 2 * ds::S3;
                    let tip = Rect::new(f.x + ds::S2, f.bottom() + ds::S1, w, KEY_H + ds::S1);
                    cv.shadow(tip, ds::R_CTRL);
                    cv.card(tip, ds::R_CTRL, c.raised, c.error);
                    self.tx.draw_ui(cv, tip.x + ds::S3, tip.y + 3, why, c.error, tip, false);
                }
            }
            Overlay::Prompt(p) => {
                let r = Rect::new(base.x, base.y, base.w, 86 + FOOT_H);
                self.modal_panel(cv, r, Elev::Modal);
                self.tx.draw_ui(cv, r.x + 16, r.y + 12, &p.title, c.tab_active_fg, r, true);
                let input = Rect::new(r.x + 12, r.y + 38, r.w - 24, 34);
                cv.card(input, ds::R_ROW, c.input_bg, c.accent);
                // New file / folder: the folder or the typed name's type icon, text shifted right of it.
                let mut tx = input.x + 10;
                if let Some(icon) = p.input_icon() {
                    self.tx.icon(cv, icon, input.x + 10, input.y + (input.h - 16) / 2, 16, dim, input);
                    tx += 24;
                }
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(Rect::new(tx - 2, input.y + 6, w + 4, 22), 4, c.accent.with_alpha(0x50));
                }
                self.tx.draw_ui(cv, tx, input.y + 8, &p.field.text, fg, input.inset(1), false);
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = tx + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, input.y + 8, 1, 18), fg, input);
                ui_modal::draw_footer(cv, &mut self.tx, &c, r, &[("Enter", "confirm"), ("Esc", "cancel")]);
            }
            Overlay::Picker(p) => {
                let shown = p.filtered.len().min(PICKER_ROWS);
                let open_docs: Vec<&std::path::Path> = self.docs.iter().filter_map(|d| d.path()).collect();
                let groups = p.groups(&open_docs);
                let calm = shown == 0 && p.kind == PickKind::Problems && p.field.text.is_empty();
                let list_h = if calm {
                    128
                } else if shown == 0 {
                    112
                } else {
                    shown as i32 * ROW + 14 + groups::reserve(&groups)
                };
                // Project search with the replace row: the list starts below it.
                let top = INPUT_H + if p.rep.is_some() { preplace::REP_H } else { 0 };
                let r = Rect::new(base.x, base.y, base.w, top + list_h + FOOT_H);
                self.modal_panel(cv, r, Elev::Modal);
                // Input row.
                // Quick actions show a lightbulb, local history a clock, everything else a magnifier.
                let lead_icon = match p.kind {
                    PickKind::CodeActions => "ui/sparkle",
                    PickKind::Locations if p.title.starts_with("Local history") => "ui/clock",
                    _ => "hugeicons/search",
                };
                self.tx.icon(cv, lead_icon, r.x + 18, r.y + (INPUT_H - 16) / 2, 16, dim, r);
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
                let text_clip = match chips {
                    Some(ch) => Rect::new(r.x, r.y, ch[0].x - 8 - r.x, INPUT_H),
                    None => r,
                };
                self.tx.draw_ui(cv, r.x + 46, r.y + (INPUT_H - 18) / 2, shown_text, col, text_clip, false);
                if let Some(ch) = chips {
                    let outer = Rect::new(ch[0].x - 2, ch[0].y - 2, ch[1].right() - ch[0].x + 4, ch[0].h + 4);
                    cv.card(outer, 8, c.input_bg, c.border);
                    for (k, chip) in ch.iter().enumerate() {
                        let on = (k == 1) == project;
                        if on {
                            cv.fill_round(*chip, 6, c.list_active_bg);
                        }
                        let label = crate::app_problems::SCOPE_LABELS[k];
                        let lw = self.tx.ui_width(label, false);
                        let ty = chip.y + (chip.h - 18) / 2;
                        self.tx.draw_ui(
                            cv,
                            chip.x + (chip.w - lw) / 2,
                            ty,
                            label,
                            if on { fg } else { dim },
                            *chip,
                            false,
                        );
                    }
                }
                if p.kind == PickKind::ProjectSearch {
                    let tag = [(p.case, "Aa"), (p.word, "Word"), (p.regex, ".*")]
                        .iter()
                        .filter(|(on, _)| *on)
                        .map(|(_, t)| *t)
                        .collect::<Vec<_>>()
                        .join("  ");
                    let tw = self.tx.ui_width(&tag, true);
                    self.tx.draw_ui(cv, r.right() - tw - 16, r.y + (INPUT_H - 18) / 2, &tag, c.accent_text, r, true);
                }
                if p.rep.as_ref().is_none_or(|rp| !rp.focus) {
                    let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                    let x = r.x + 46 + self.tx.ui_width(&prefix, false);
                    cv.fill_clip(Rect::new(x, r.y + (INPUT_H - 20) / 2, 1, 20), fg, r);
                }
                cv.fill(Rect::new(r.x + 16, r.y + INPUT_H - 1, r.w - 32, 1), c.border);
                if p.rep.is_some() {
                    preplace::draw_replace_row(cv, &mut self.tx, &self.theme, r, p);
                    cv.fill(Rect::new(r.x + 16, r.y + top - 1, r.w - 32, 1), c.border);
                }

                let mut rows = Vec::new();
                // Mid-glide the list sits a fraction of a row past `p.scroll`: rows shift up by `off`
                // px, one more row shows at the bottom and everything is clipped to the list.
                let off = if groups.is_empty() { self.region_off(crate::scrollreg::Reg::Picker, ROW) } else { 0 };
                let lclip = Rect::new(r.x, r.y + top, r.w, list_h);
                if !p.filtered.is_empty() {
                    // The highlight glides between rows; the accent bar marks the selected one.
                    let glide = groups::glide_y(&groups, p.scroll, self.modal.sel.value, ROW);
                    let hl = Rect::new(r.x + 8, r.y + top + 6 + glide - off, r.w - 16, ROW);
                    cv.fill_round_clip(hl, ds::R_ROW + 2, c.list_active_bg, lclip);
                    cv.fill_round_clip(Rect::new(hl.x + 3, hl.y + 9, 3, ROW - 18), 1, c.accent, lclip);
                }
                // Group headers: each one above its first row, the one of the first visible row sticky.
                if !groups.is_empty() {
                    let list_y = r.y + top + 6;
                    let mut at: Vec<(i32, &groups::Group)> = Vec::new();
                    if let Some(g) = groups::group_of(&groups, p.scroll).filter(|g| g.start < p.scroll) {
                        at.push((list_y, g));
                    }
                    for g in groups.iter().filter(|g| g.start >= p.scroll && g.start < p.scroll + PICKER_ROWS) {
                        at.push((list_y + groups::row_y(&groups, p.scroll, g.start, ROW) - groups::HDR_H, g));
                    }
                    for (hy, g) in at {
                        let ty = hy + (groups::HDR_H - 14) / 2 + 3;
                        self.tx.draw_ui_sized(cv, r.x + 22, ty, &g.title.to_uppercase(), dim, lclip, true, 10.5);
                        let tw = self.tx.ui_width_sized(&g.title.to_uppercase(), true, 10.5);
                        let n = g.count.to_string();
                        self.tx.draw_ui_sized(
                            cv,
                            r.x + 22 + tw + ds::S2,
                            ty,
                            &n,
                            dim.with_alpha(0xa0),
                            lclip,
                            false,
                            10.5,
                        );
                    }
                }
                for (k, (idx, pos)) in
                    p.filtered.iter().enumerate().skip(p.scroll).take(PICKER_ROWS + (off > 0) as usize)
                {
                    let it = &p.items[*idx];
                    let y = r.y + top + 6 + groups::row_y(&groups, p.scroll, k, ROW) - off;
                    let row_full = Rect::new(r.x + 8, y, r.w - 16, ROW);
                    let rr = row_full.intersect(&lclip);
                    if let Some(h) = p.rep.as_ref().and_then(|rp| rp.hits.get(*idx)) {
                        preplace::draw_preview(cv, &mut self.tx, &self.theme, rr, y + (ROW - 18) / 2, h, &it.detail);
                        rows.push((rr, k));
                        continue;
                    }
                    let avail = rr.w - 28;
                    let cmd_keys = if p.kind == PickKind::Commands {
                        crate::ui_kit::keycap::keycaps(&it.detail)
                    } else {
                        Vec::new()
                    };
                    let detail_w = if p.kind == PickKind::Commands {
                        if cmd_keys.is_empty() {
                            0
                        } else {
                            crate::ui_kit::keycap::keys_width(&mut self.tx, &cmd_keys) + 20
                        }
                    } else if matches!(p.kind, PickKind::Themes | PickKind::Choice) && !it.detail.is_empty() {
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
                    // Paths dim their folders; commands dim their area ("File: ").
                    let dir_end = match p.kind {
                        PickKind::QuickOpen => label.iter().rposition(|ch| *ch == '/').map_or(0, |i| i + 1),
                        PickKind::Commands => label
                            .iter()
                            .position(|ch| *ch == ':')
                            .filter(|i| label.get(i + 1) == Some(&' '))
                            .map_or(0, |i| i + 2),
                        _ => 0,
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
                            // The shortcut as keycap chips, right aligned.
                            let kw = detail_w - 20;
                            let ky = y + (ROW - KEY_H) / 2;
                            crate::ui_kit::keycap::draw_keys_in(
                                cv,
                                &mut self.tx,
                                &c,
                                rr.right() - kw - 12,
                                ky,
                                &cmd_keys,
                                1.0,
                                rr,
                            );
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
                        Some((Rect::new(r.x, r.y + top, r.w, list_h), p.filtered.len() as f64, p.scroll as f64));
                }
                if p.filtered.is_empty() && calm {
                    // Calm empty state: a check mark, a title and which scope was looked at.
                    let area = Rect::new(r.x, r.y + top, r.w, list_h);
                    let hint =
                        if project { "Alt+W: look at the open files only" } else { "Alt+W: look at the whole project" };
                    self.tx.icon(cv, "ui/check", area.x + (area.w - 28) / 2, area.y + 14, 28, c.git_added, area);
                    let title = "No problems";
                    let tw = self.tx.ui_width(title, true);
                    self.tx.draw_ui(cv, area.x + (area.w - tw) / 2, area.y + 54, title, c.tab_active_fg, area, true);
                    let hw = self.tx.ui_width(hint, false);
                    self.tx.draw_ui(cv, area.x + (area.w - hw) / 2, area.y + 80, hint, dim, area, false);
                } else if p.filtered.is_empty() {
                    // Friendly empty state: an icon, what happened, and what to try.
                    let area = Rect::new(r.x, r.y + top, r.w, list_h);
                    let (title, hint) = empty_copy(p.kind, &p.field.text);
                    self.tx.icon(cv, "hugeicons/search", area.x + (area.w - 24) / 2, area.y + 14, 24, dim, area);
                    let tw = self.tx.ui_width(title, true);
                    self.tx.draw_ui(cv, area.x + (area.w - tw) / 2, area.y + 46, title, c.tab_active_fg, area, true);
                    let hint = crate::ui::fit_ellipsis(&mut self.tx, hint, area.w - 2 * ds::S5, false);
                    let hw = self.tx.ui_width(&hint, false);
                    self.tx.draw_ui(cv, area.x + (area.w - hw) / 2, area.y + 70, &hint, dim, area, false);
                }
                let hints = if p.title.starts_with("Local history") { HIST_HINTS } else { PICK_HINTS };
                ui_modal::draw_footer(cv, &mut self.tx, &c, r, &hints);
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
        if let Some(snap) = reveal {
            self.reveal_end(cv, snap);
        }
    }
}
