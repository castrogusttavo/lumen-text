//! App side of the editing aids in `assist`: the cursor-rest debounce that asks the language
//! servers for document highlights and linked-editing ranges (or falls back to the same word),
//! their answers, the `rulers` / `inline_diagnostics` settings and the palette command.

use crate::{
    app::App,
    assist::{self, HlReq, InlineDiags, Linked, WordHl},
    doc::Doc,
    lsp::Pending,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long the caret rests on a symbol before its occurrences are asked for.
const HL_DEBOUNCE: Duration = Duration::from_millis(150);

impl App {
    /// Give a freshly opened document the configured rulers and inline-diagnostics mode.
    pub(crate) fn assist_setup(&self, d: &mut Doc) {
        d.assist.rulers = assist::merge_rulers(&self.config.rulers_for(d.lang), d.fileconf.max_line_length);
        d.assist.inline = InlineDiags::from_name(&self.config.inline_diagnostics);
    }

    /// The caret moved or the text changed in the active document: drop highlights that no longer
    /// apply (and any answer still on its way), then ask again once the caret rests.
    pub(crate) fn assist_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let i = self.active;
        let Some(d) = self.docs.get_mut(i) else { return };
        let (at, ver) = (d.ed.head(), d.buf.version());
        if d.assist.linked.as_ref().is_some_and(|l| l.ver != ver || !l.contains(at)) {
            d.assist.linked = None;
        }
        let mut redraw = false;
        if d.assist.hl_key != Some((at, ver)) {
            d.assist.hl_key = Some((at, ver));
            // Moving inside the same occurrence keeps what is shown (and asks nothing).
            if !d.assist.caret_in_hl(at, ver, &d.buf) {
                redraw = d.assist.clear_hl();
                d.assist.hl_due = Some(now + HL_DEBOUNCE);
            }
        }
        let fire = match d.assist.hl_due {
            Some(t) if t > now => {
                want(t);
                false
            }
            Some(_) => {
                d.assist.hl_due = None;
                true
            }
            None => false,
        };
        if fire {
            self.assist_fire(i, now, want);
        }
        if redraw {
            self.request_redraw();
        }
    }

    /// The caret rests: ask the servers of document `i` for the occurrences of the symbol under it
    /// (every server that offers them; the primary's answer wins) and for linked ranges (the first
    /// server that offers them), or highlight the same word when no server can.
    fn assist_fire(&mut self, i: usize, now: Instant, want: &mut impl FnMut(Instant)) {
        let d = &self.docs[i];
        if !d.ed.sel.is_empty() || !d.ed.extra.is_empty() {
            return;
        }
        let at = d.ed.head();
        let Some((ws, we)) = assist::word_range(&d.buf, at) else { return };
        let ver = d.buf.version();
        let primary = self.doc_lsp(i).filter(|&li| d.lsp_open && self.lsps[li].ready);
        let mut servers: Vec<usize> = primary.into_iter().collect();
        if self.lsp_enabled() {
            servers.extend(self.lspx_open_companions(i));
        }
        let hl: Vec<usize> =
            servers.iter().copied().filter(|&li| self.lsps[li].has_cap("documentHighlightProvider")).collect();
        let linked = servers.iter().copied().find(|&li| self.lsps[li].has_cap("linkedEditingRangeProvider"));
        if hl.is_empty() {
            let word = d.buf.slice(ws, we);
            let d = &mut self.docs[i];
            d.assist.hl_word = Some(word);
            d.assist.hl_ver = ver;
            self.request_redraw();
        }
        if hl.is_empty() && linked.is_none() {
            return;
        }
        // The servers must hold this exact text before they are asked about it.
        self.flush(i);
        if self.docs[i].changed_at.is_some() {
            // Not synced yet (a companion-only document between debounces): try again shortly.
            let t = now + HL_DEBOUNCE;
            self.docs[i].assist.hl_due = Some(t);
            want(t);
            return;
        }
        let d = &mut self.docs[i];
        let (Some(real), Some(main_uri)) = (d.real_uri(), d.uri()) else { return };
        let (l, c) = d.lsp_pos(at);
        let params = |uri: &str| json!({"textDocument": {"uri": uri}, "position": {"line": l, "character": c}});
        let uri_for = |li: usize| if Some(li) == primary { main_uri.as_str() } else { real.as_str() };
        d.assist.serial += 1;
        let serial = d.assist.serial;
        if !hl.is_empty() {
            d.assist.hl_req = Some(HlReq { serial, ver, at, expected: hl.len(), parts: Vec::new() });
            for li in hl {
                let order = if Some(li) == primary { 0 } else { 1 + li };
                let what = Pending::Highlight { uri: real.clone(), serial, order };
                self.lsps[li].request("textDocument/documentHighlight", params(uri_for(li)), what);
            }
        }
        if let Some(li) = linked {
            let what = Pending::LinkedEditing { uri: real.clone(), serial, ver, at };
            self.lsps[li].request("textDocument/linkedEditingRange", params(uri_for(li)), what);
        }
    }

    /// An answer to `assist_fire` (a failed request counts as an empty answer).
    pub(crate) fn assist_on_response(&mut self, li: usize, what: Pending, result: Result<Value, Value>) {
        let v = result.unwrap_or(Value::Null);
        let name = self.lsps.get(li).map(|l| l.name.clone()).unwrap_or_default();
        match what {
            Pending::Highlight { uri, serial, order } => {
                let Some(i) = self.doc_by_uri(&uri) else { return };
                if self.assist_take_highlights(i, serial, order, name, v) {
                    self.request_redraw();
                }
            }
            Pending::LinkedEditing { uri, serial, ver, at } => {
                let Some(i) = self.doc_by_uri(&uri) else { return };
                let d = &mut self.docs[i];
                if d.assist.serial != serial || d.buf.version() != ver || d.ed.head() != at {
                    return;
                }
                d.assist.linked = linked_from(d, &v, ver, at);
            }
            _ => {}
        }
    }

    /// Record one server's highlights; once every server answered, keep the merged ones if the
    /// document and caret are still where the request was made. True when highlights changed.
    fn assist_take_highlights(&mut self, i: usize, serial: u64, order: usize, name: String, v: Value) -> bool {
        let d = &mut self.docs[i];
        let Some(req) = d.assist.hl_req.as_mut().filter(|r| r.serial == serial) else { return false };
        req.parts.push((order, name, v));
        req.expected = req.expected.saturating_sub(1);
        if req.expected > 0 {
            return false;
        }
        let Some(req) = d.assist.hl_req.take() else { return false };
        if d.buf.version() != req.ver || d.ed.head() != req.at {
            return false;
        }
        let merged = crate::lspmerge::merge_highlights(&req.parts);
        let mut hl: Vec<WordHl> = assist::parse_highlights(&merged)
            .into_iter()
            .map(|(sl, sc, el, ec, write)| WordHl { a: d.pos_from_lsp(sl, sc), b: d.pos_from_lsp(el, ec), write })
            .filter(|h| h.a < h.b)
            .collect();
        hl.sort_by_key(|h| h.a);
        d.assist.hl = hl;
        d.assist.hl_word = None;
        d.assist.hl_ver = req.ver;
        true
    }

    /// `rulers` / `inline_diagnostics` from the settings modal or the palette; false for other keys.
    pub(crate) fn apply_assist_setting(&mut self, key: &str, v: &Value) -> bool {
        match key {
            "rulers" => self.config.rulers = assist::parse_rulers(v),
            "inline_diagnostics" => {
                self.config.inline_diagnostics = InlineDiags::from_name(v.as_str().unwrap_or("")).name().to_string()
            }
            _ => return false,
        }
        let mut docs = std::mem::take(&mut self.docs);
        for d in &mut docs {
            self.assist_setup(d);
        }
        self.docs = docs;
        self.request_redraw();
        true
    }

    /// Palette: inline diagnostics off -> current line -> all lines -> off.
    pub fn cycle_inline_diagnostics(&mut self) {
        let next = InlineDiags::from_name(&self.config.inline_diagnostics).next();
        self.apply_setting("inline_diagnostics", &json!(next.name()));
        self.say(format!("inline diagnostics: {}", next.label()));
    }
}

/// Linked ranges from a `linkedEditingRange` answer, kept only when they make sense for the text
/// the request was made on: same text in every range, no overlap, the caret inside one of them.
fn linked_from(d: &Doc, v: &Value, ver: u64, at: usize) -> Option<Linked> {
    let (rs, pattern) = assist::parse_linked(v)?;
    let mut ranges: Vec<(usize, usize)> =
        rs.iter().map(|&(sl, sc, el, ec)| (d.pos_from_lsp(sl, sc), d.pos_from_lsp(el, ec))).collect();
    ranges.sort_unstable();
    let first = d.buf.slice(ranges[0].0, ranges[0].1);
    let sane = ranges.iter().all(|&(a, b)| a <= b && d.buf.slice(a, b) == first)
        && ranges.windows(2).all(|w| w[0].1 < w[1].0)
        && pattern.allows(&first);
    let l = Linked { ranges, pattern, ver };
    (sane && l.contains(at)).then_some(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};
    use crate::ui::Canvas;
    use std::path::{Path, PathBuf};

    fn fake_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let p = exe.parent()?.parent()?.join("examples").join(format!("fake_lsp{}", std::env::consts::EXE_SUFFIX));
        p.exists().then_some(p)
    }

    fn project(tag: &str) -> PathBuf {
        let d = scratch_base().join(format!("assist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    /// Sync tick, assist tick and message pump until `cond` holds (or give up after `ms`).
    fn pump(app: &mut App, ms: u64, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            app.lsp_tick(Instant::now(), &mut |_| {});
            app.assist_tick(Instant::now(), &mut |_| {});
            app.pump_lsp();
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        false
    }

    fn server(exe: &Path, name: &str, role: &str, env: Value) -> Value {
        let mut env = env;
        env["FAKE_NAME"] = json!(name);
        json!({"command": exe, "languages": ["fk"], "role": role, "env": env})
    }

    fn open(app: &mut App, f: &Path, text: &str) -> usize {
        std::fs::write(f, text).unwrap();
        let i = app.open_path(f, None).expect("open");
        app.active = i;
        i
    }

    fn caret(app: &mut App, i: usize, line: usize, col: usize) {
        let d = &mut app.docs[i];
        let p = d.buf.pos(line, col);
        d.ed.click(&mut d.buf, p, false);
    }

    fn stop(app: &mut App) {
        for l in &mut app.lsps {
            l.shutdown();
        }
    }

    #[test]
    fn highlights_ask_every_server_primary_wins_and_linked_tags_follow() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("hl");
        let mut app = test_app(&dir);
        // The primary answers only the first occurrence, the companion all three: the primary wins.
        app.config.language_servers = json!({
            "fake-a": server(&exe, "a", "primary", json!({"FAKE_HIGHLIGHT": "first", "FAKE_LINKED": "1"})),
            "fake-b": server(&exe, "b", "companion", json!({"FAKE_HIGHLIGHT": "1"})),
        });
        let i = open(&mut app, &dir.join("x.fk"), "let foo = 1;\nfoo = foo + <div>x</div>;\n");
        assert!(
            pump(&mut app, 8000, |a| a.lsps.len() == 2
                && a.lsps.iter().all(|l| l.ready)
                && a.docs[i].lsp_open
                && a.lspx_open_companions(i).len() == 1),
            "servers not ready: {:?}",
            app.status
        );
        caret(&mut app, i, 1, 1); // on `foo`
        assert!(pump(&mut app, 4000, |a| !a.docs[i].assist.hl.is_empty()), "no highlights");
        let hl = app.docs[i].assist.hl.clone();
        assert_eq!(hl, vec![WordHl { a: 4, b: 7, write: true }], "the primary's answer wins");
        for l in &app.lsps {
            assert!(l.log_text().contains("textDocument/documentHighlight"), "{} was not asked", l.name);
        }
        // Moving inside the same occurrence keeps them; an edit drops them at once.
        caret(&mut app, i, 0, 6);
        app.assist_tick(Instant::now(), &mut |_| {});
        assert_eq!(app.docs[i].assist.hl, hl);
        let d = &mut app.docs[i];
        d.ed.type_char(&mut d.buf, 'o');
        d.sync_edits();
        app.assist_tick(Instant::now(), &mut |_| {});
        assert!(app.docs[i].assist.hl.is_empty(), "an edit cancels the highlights");
        let d = &mut app.docs[i];
        d.ed.undo(&mut d.buf);
        d.sync_edits();

        // Linked editing: the caret in `div` of the opening tag links it to the closing one.
        caret(&mut app, i, 1, 14);
        assert!(pump(&mut app, 4000, |a| a.docs[i].assist.linked.is_some()), "no linked ranges");
        caret(&mut app, i, 1, 16); // end of the opening name, still inside the link
        app.assist_tick(Instant::now(), &mut |_| {});
        let d = &mut app.docs[i];
        assert!(d.assist.linked.is_some());
        d.ed.type_char(&mut d.buf, 'v');
        d.sync_edits();
        assert_eq!(d.buf.line(1), "foo = foo + <divv>x</divv>;");
        d.ed.undo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.line(1), "foo = foo + <div>x</div>;", "one undo step for both names");
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_server_the_same_word_is_highlighted_after_the_caret_rests() {
        let _g = lock();
        let dir = project("word");
        let mut app = test_app(&dir);
        let i = open(&mut app, &dir.join("w.txt"), "alpha beta alpha\nalphabet alpha\n");
        caret(&mut app, i, 0, 2);
        let t0 = Instant::now();
        let mut next = None;
        app.assist_tick(t0, &mut |t| next = Some(t));
        assert_eq!(next, Some(t0 + HL_DEBOUNCE), "debounced");
        assert!(app.docs[i].assist.hl_word.is_none());
        app.assist_tick(t0 + HL_DEBOUNCE, &mut |_| {});
        assert_eq!(app.docs[i].assist.hl_word.as_deref(), Some("alpha"));
        let ver = app.docs[i].buf.version();
        assert_eq!(app.docs[i].assist.live_hl(ver).1, Some("alpha"));
        // Off a word: nothing; a selection: nothing (the selection's own occurrences show instead).
        caret(&mut app, i, 0, 5);
        app.assist_tick(t0, &mut |_| {});
        assert_eq!(app.docs[i].assist.hl_word.as_deref(), Some("alpha"), "still touching `alpha`");
        let d = &mut app.docs[i];
        d.ed.sel = crate::buffer::Sel { anchor: 6, head: 10 };
        app.assist_tick(t0, &mut |_| {});
        app.assist_tick(t0 + HL_DEBOUNCE, &mut |_| {});
        assert!(app.docs[i].assist.hl_word.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linked_answers_that_do_not_fit_the_text_are_ignored() {
        let d = Doc::new(crate::buffer::Buffer::from_str("<a>x</b>"), None, Some("html"), "//", 8.0);
        let r = |a: u64, b: u64| json!({"start": {"line": 0, "character": a}, "end": {"line": 0, "character": b}});
        let v = json!({"ranges": [r(1, 2), r(6, 7)]});
        assert!(linked_from(&d, &v, 0, 1).is_none(), "different names");
        let d = Doc::new(crate::buffer::Buffer::from_str("<a>x</a>"), None, Some("html"), "//", 8.0);
        assert!(linked_from(&d, &v, 0, 4).is_none(), "caret outside both ranges");
        let l = linked_from(&d, &v, 0, 7).expect("caret in the closing name");
        assert_eq!(l.ranges, vec![(1, 2), (6, 7)]);
    }

    /// Render the app twice (before / after `change`) and return both frames.
    fn frames(app: &mut App, change: impl FnOnce(&mut App)) -> (Vec<u32>, Vec<u32>) {
        let (w, h) = (app.size.0 as i32, app.size.1 as i32);
        let mut a = vec![0u32; (w * h) as usize];
        app.render_into(&mut Canvas { px: &mut a, w, h }, 0.016);
        app.render_into(&mut Canvas { px: &mut a, w, h }, 0.016);
        change(app);
        let mut b = vec![0u32; (w * h) as usize];
        app.render_into(&mut Canvas { px: &mut b, w, h }, 0.016);
        (a, b)
    }

    /// Pixels that differ between two frames inside `[x0, x1) x [y0, y1)`.
    fn diff(a: &[u32], b: &[u32], w: i32, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> usize {
        let mut n = 0;
        for y in y0.max(0)..y1 {
            for x in x0.max(0)..x1.min(w) {
                let k = (y * w + x) as usize;
                n += usize::from(a.get(k) != b.get(k));
            }
        }
        n
    }

    #[test]
    fn rulers_draw_one_thin_line_at_their_column() {
        let _g = lock();
        let dir = project("rulers");
        let mut app = test_app(&dir);
        app.size = (900, 400);
        let i = open(&mut app, &dir.join("r.txt"), "short\n");
        let (a, b) = frames(&mut app, |app| app.apply_setting("rulers", &json!("40")));
        assert_eq!(app.config.rulers, vec![40]);
        assert_eq!(app.docs[i].assist.rulers, vec![40], "applied live to open documents");
        let d = &app.docs[i];
        let x = d.rect.x + d.gutter_w() + (40.0 * d.cell_w).round() as i32 - d.left.round() as i32;
        let w = app.size.0 as i32;
        let rows = (d.rect.y + 40, d.rect.bottom() - 20);
        assert_eq!(diff(&a, &b, w, (x, rows.0, x + 1, rows.1)), (rows.1 - rows.0) as usize, "a full-height line");
        assert_eq!(diff(&a, &b, w, (d.rect.x, rows.0, x, rows.1)), 0, "nothing left of it");
        assert_eq!(diff(&a, &b, w, (x + 1, rows.0, d.rect.right() - 20, rows.1)), 0, "nothing right of it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_diagnostic_sits_after_the_code_and_never_over_it() {
        let _g = lock();
        let dir = project("inline");
        let mut app = test_app(&dir);
        app.size = (900, 400);
        let long = "x".repeat(400);
        let i = open(&mut app, &dir.join("d.txt"), &format!("let a = 1;\n{long}\nok\n"));
        app.apply_setting("inline_diagnostics", &json!("off"));
        let diag = |l: usize, msg: &str| crate::doc::Diag {
            sl: l,
            sc: 0,
            el: l,
            ec: 3,
            severity: 1,
            message: msg.into(),
            code: String::new(),
            source: String::new(),
        };
        app.docs[i].set_source_diags(false, vec![diag(0, "Cannot find name 'a'."), diag(1, "Too long.")]);
        caret(&mut app, i, 2, 0); // the caret's line has no problem
        let (a, b) = frames(&mut app, |app| app.apply_setting("inline_diagnostics", &json!("all")));
        let d = &app.docs[i];
        let (lh, w) = (crate::ui::line_h() as i32, app.size.0 as i32);
        let text_x = d.rect.x + d.gutter_w();
        let end = text_x + (10.0 * d.cell_w).round() as i32; // "let a = 1;"
        let row0 = (d.rect.y + 2, d.rect.y + lh - 4); // above the squiggle
        assert!(diff(&a, &b, w, (end + 1, row0.0, d.rect.right() - 20, row0.1)) > 20, "message after the code");
        assert_eq!(diff(&a, &b, w, (d.rect.x, row0.0, end + 1, row0.1)), 0, "the code is untouched");
        let row1 = (d.rect.y + lh, d.rect.y + 2 * lh);
        assert_eq!(
            diff(&a, &b, w, (d.rect.x, row1.0, d.rect.right(), row1.1)),
            0,
            "a line filling the row shows nothing"
        );
        let area = (d.rect.x, d.rect.y, d.rect.right(), d.rect.bottom());
        // Current line only: the caret's line has no problem, so nothing shows anywhere.
        let (a2, b2) = frames(&mut app, |app| app.apply_setting("inline_diagnostics", &json!("current_line")));
        assert!(diff(&a2, &b2, w, (area.0, row0.0, area.2, row0.1)) > 20, "line 0 message gone");
        let mut px = vec![0u32; (w * app.size.1 as i32) as usize];
        app.render_into(&mut Canvas { px: &mut px, w, h: app.size.1 as i32 }, 0.016);
        assert_eq!(diff(&a, &px, w, area), 0, "current line without problems = off");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn palette_cycles_the_inline_mode_and_saves_it() {
        let _g = lock();
        let dir = project("cycle");
        let mut app = test_app(&dir);
        assert_eq!(app.config.inline_diagnostics, "current_line");
        app.run_cmd(crate::app_overlay::Cmd::CycleInlineDiagnostics);
        assert_eq!(app.config.inline_diagnostics, "all");
        assert!(app.docs.iter().all(|d| d.assist.inline == InlineDiags::All));
        app.run_cmd(crate::app_overlay::Cmd::CycleInlineDiagnostics);
        assert_eq!(app.config.inline_diagnostics, "off");
        let saved = crate::config::Config::load_from(&dir.join(".config/lumen-text/config.json"));
        assert_eq!(saved.inline_diagnostics, "off");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
