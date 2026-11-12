//! Glue for the file-format features: `.editorconfig` applied when a file opens, the encoding
//! item of the bottom bar with its "Reopen / Save with encoding…" pickers, the whitespace fixes
//! made on save and the "Toggle render whitespace" command. The logic lives in `encoding`,
//! `editorconfig` and `whitespace`.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay},
    buffer::{EditKind, Sel},
    doc::Doc,
    editorconfig::{Eol, IndentStyle},
    encoding::{CHOICES, Kind, TextEncoding},
    whitespace::{Fix, Render},
};
use serde_json::json;
use std::{
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
};

/// What `.editorconfig` says about a document that was just opened: indentation and tab width,
/// the line ending of a file that has no line break yet, and `charset` as the encoding it is
/// saved in (when that does not contradict what the file is: an ASCII or empty file takes it,
/// a UTF-16 file does not become Latin-1). Session picks are applied later and win.
pub fn apply_editorconfig(doc: &mut Doc) {
    let Some(path) = doc.path().map(Path::to_path_buf) else { return };
    let p = crate::editorconfig::props_for(&path);
    if let Some(w) = p.tab_width {
        doc.ed.tab_w = w.clamp(1, 16);
    }
    let size = p.indent_size.map(|n| n.clamp(1, 16));
    match p.indent_style {
        Some(IndentStyle::Tab) => doc.ed.indent = "\t".into(),
        Some(IndentStyle::Space) => {
            let n = size.unwrap_or(if doc.ed.indent.contains('\t') { doc.ed.tab_w } else { doc.ed.indent.len() });
            doc.ed.indent = " ".repeat(n.max(1));
        }
        None => {
            if let Some(n) = size.filter(|_| !doc.ed.indent.contains('\t')) {
                doc.ed.indent = " ".repeat(n);
            }
        }
    }
    // Only a file without any line break: an existing one keeps the endings it has.
    if let Some(eol) = p.end_of_line
        && doc.buf.line_count() <= 1
    {
        doc.buf.crlf = eol == Eol::Crlf;
    }
    if let Some(want) = p.charset.as_deref().and_then(TextEncoding::from_id) {
        let cur = doc.buf.encoding;
        let empty = doc.buf.len_chars() == 0;
        let ascii = cur == TextEncoding::UTF8 && doc.buf.len_bytes() == doc.buf.len_chars();
        if empty || cur.kind == want.kind || (ascii && matches!(want.kind, Kind::Utf8 | Kind::Latin1)) {
            doc.buf.encoding = want;
        }
    }
    doc.fileconf = p;
}

/// Is this a Markdown document (where two trailing spaces are a line break)?
fn is_markdown(d: &Doc) -> bool {
    d.lang == Some("markdown")
        || d.path()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx"))
}

/// Apply save fixes to `d` as one undo step; every cursor and selection follows the text.
fn apply_fixes(d: &mut Doc, edits: &[Fix]) {
    let map = |s: Sel| Sel {
        anchor: crate::whitespace::map_pos(edits, s.anchor),
        head: crate::whitespace::map_pos(edits, s.head),
    };
    let before = d.ed.sel;
    d.buf.close_group();
    d.buf.begin_compound(before);
    // Last first, so the earlier positions stay valid.
    for (s, e, t) in edits.iter().rev() {
        d.buf.replace(*s, *e, t, EditKind::Other, before);
    }
    d.ed.sel = map(before);
    for x in d.ed.extra.iter_mut() {
        *x = map(*x);
    }
    d.buf.end_compound(d.ed.sel);
    d.sync_edits();
}

/// Last mode "Toggle render whitespace" turned off, to bring back next time (default: all).
static LAST_WS: AtomicU8 = AtomicU8::new(3);

impl App {
    /// Before writing document `i`: trim trailing whitespace and add the final line break when
    /// the settings or the file's `.editorconfig` ask for it. Markdown is not trimmed unless
    /// `.editorconfig` says so. One undo step; cursors in trailing whitespace stay put.
    pub fn whitespace_on_save(&mut self, i: usize) {
        let (cfg_trim, cfg_nl) = (self.config.trim_trailing_whitespace, self.config.insert_final_newline);
        let Some(d) = self.docs.get_mut(i) else { return };
        let trim = d.fileconf.trim_trailing_whitespace.unwrap_or(cfg_trim && !is_markdown(d));
        let final_nl = d.fileconf.insert_final_newline.unwrap_or(cfg_nl);
        if !trim && !final_nl {
            return;
        }
        let cursors: Vec<usize> = std::iter::once(d.ed.sel.head).chain(d.ed.extra.iter().map(|s| s.head)).collect();
        let edits = crate::whitespace::save_edits(&d.buf, trim, final_nl, &cursors);
        if !edits.is_empty() {
            apply_fixes(d, &edits);
        }
    }

    /// Palette "Toggle render whitespace": off, or back to the last mode that was on.
    pub fn toggle_render_whitespace(&mut self) {
        let cur = Render::from_name(&self.config.render_whitespace);
        let next = if cur == Render::Off {
            match LAST_WS.load(Ordering::Relaxed) {
                1 => Render::Boundary,
                2 => Render::Trailing,
                _ => Render::All,
            }
        } else {
            LAST_WS.store(
                match cur {
                    Render::Boundary => 1,
                    Render::Trailing => 2,
                    _ => 3,
                },
                Ordering::Relaxed,
            );
            Render::Off
        };
        self.apply_setting("render_whitespace", &json!(next.name()));
        self.say(format!("render whitespace: {}", next.name()));
        self.request_redraw();
    }

    /// Bottom bar encoding: choose between reopening the file in another encoding and saving it
    /// in one.
    pub fn open_encoding_menu(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let cur = d.buf.encoding.label();
        let items = vec![
            Item {
                label: "Reopen with encoding…".into(),
                detail: "read the file again, decoded another way".into(),
                action: Action::Encoding("reopen".into()),
            },
            Item {
                label: "Save with encoding…".into(),
                detail: "write the file in another encoding".into(),
                action: Action::Encoding("save".into()),
            },
        ];
        self.overlay = Overlay::Picker(crate::app_overlay::choice_picker(&format!("File encoding: {cur}"), items, 0));
        self.request_redraw();
    }

    /// The list of encodings, for reopening (`save = false`) or saving the active document.
    pub fn open_encoding_picker(&mut self, save: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        let cur = d.buf.encoding.id();
        let verb = if save { "save" } else { "reopen" };
        let items: Vec<Item> = CHOICES
            .iter()
            .map(|(id, label)| Item {
                label: label.to_string(),
                detail: if *id == cur { format!("{id} · current") } else { id.to_string() },
                action: Action::Encoding(format!("{verb}:{id}")),
            })
            .collect();
        let sel = CHOICES.iter().position(|(id, _)| *id == cur).unwrap_or(0);
        let title = if save { "Save with encoding (type to filter)" } else { "Reopen with encoding (type to filter)" };
        self.overlay = Overlay::Picker(crate::app_overlay::choice_picker(title, items, sel));
        self.request_redraw();
    }

    /// Run a choice of the encoding pickers: `reopen` / `save` open the list, `reopen:<id>` and
    /// `save:<id>` do it.
    pub fn encoding_action(&mut self, spec: &str) {
        match spec.split_once(':') {
            None if spec == "save" => self.open_encoding_picker(true),
            None => self.open_encoding_picker(false),
            Some((verb, id)) => {
                let Some(enc) = TextEncoding::from_id(id) else { return self.say(format!("unknown encoding: {id}")) };
                if verb == "save" { self.save_with_encoding(enc) } else { self.reopen_with_encoding(enc) }
            }
        }
    }

    /// Read the active file again decoded as `enc` (refused while it has unsaved changes, which
    /// the reload would throw away).
    pub fn reopen_with_encoding(&mut self, enc: TextEncoding) {
        let i = self.active;
        let Some(d) = self.docs.get_mut(i) else { return };
        if d.path().is_none() {
            return self.say("this document has no file to reopen yet");
        }
        if d.buf.is_dirty() {
            return self.say("Unsaved changes — save or undo them before reopening with another encoding");
        }
        let name = d.name.clone();
        match d.reload_as(Some(enc)) {
            Ok(()) => {
                d.reveal = true;
                self.say(format!("Reopened {name} as {}", enc.label()));
            }
            Err(e) => self.say(format!("Could not reopen {name} as {}: {e}", enc.label())),
        }
        self.request_redraw();
    }

    /// Save the active document in `enc` from now on. When the encoding cannot represent the
    /// text, nothing is written and the old encoding stays.
    pub fn save_with_encoding(&mut self, enc: TextEncoding) {
        let i = self.active;
        let Some(d) = self.docs.get_mut(i) else { return };
        let old = d.buf.encoding;
        d.buf.encoding = enc;
        if d.path().is_none() {
            // Untitled: the "Save as" prompt takes over and writes it in `enc`.
            return self.save_active();
        }
        let name = d.name.clone();
        match self.save_doc(i) {
            Ok(()) => {
                self.refresh_git_status();
                self.say(format!("Saved {name} as {}", enc.label()));
            }
            Err(e) => {
                if let Some(d) = self.docs.get_mut(i) {
                    d.buf.encoding = old;
                }
                self.say_for(e, 8);
            }
        }
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use crate::encoding::{Kind, TextEncoding};
    use serde_json::json;

    fn status(app: &crate::app::App) -> String {
        app.status.as_ref().map(|s| s.0.clone()).unwrap_or_default()
    }

    #[test]
    fn latin1_file_opens_shows_its_encoding_and_saves_back_the_same_bytes() {
        let _g = lock();
        let dir = tmp("ff-latin1");
        let mut app = test_app(&dir);
        let f = dir.join("old.txt");
        std::fs::write(&f, b"ma\xE7\xE3\n").unwrap();
        let i = app.open_path(&f, None).expect("a Windows-1252 file opens");
        app.active = i;
        assert_eq!(app.docs[i].buf.to_string(), "maçã\n");
        assert_eq!(app.docs[i].buf.encoding.short(), "Windows-1252");
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 4, false);
        d.ed.type_char(&mut d.buf, '!');
        app.save_active();
        assert_eq!(std::fs::read(&f).unwrap(), b"ma\xE7\xE3!\n", "written in the same encoding");
        // A character Windows-1252 lacks: the save is refused and the file keeps its bytes.
        app.docs[i].replace_text("日本\n");
        app.save_active();
        assert!(status(&app).contains("cannot represent"), "{}", status(&app));
        assert_eq!(std::fs::read(&f).unwrap(), b"ma\xE7\xE3!\n");
        assert!(app.docs[i].buf.is_dirty());
        assert!(!dir.join("old.txt.lumen-tmp").exists(), "no temp file left behind");
        // Save with encoding: UTF-8 takes it; a refused one keeps the old encoding.
        app.save_with_encoding(TextEncoding::from_id("iso-8859-1").unwrap());
        assert_eq!(app.docs[i].buf.encoding.short(), "Windows-1252", "refused: unchanged");
        app.encoding_action("save:utf-8");
        assert_eq!(std::fs::read(&f).unwrap(), "日本\n".as_bytes());
        assert_eq!(app.docs[i].buf.encoding, TextEncoding::UTF8);
        assert!(!app.docs[i].buf.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopen_with_encoding_decodes_again_and_refuses_unsaved_changes() {
        let _g = lock();
        let dir = tmp("ff-reopen");
        let mut app = test_app(&dir);
        let f = dir.join("k.txt");
        std::fs::write(&f, b"\xF0\xD2\xC9\xD7\xC5\xD4\n").unwrap(); // "Привет" in KOI8-R
        let i = app.open_path(&f, None).unwrap();
        app.active = i;
        assert_eq!(app.docs[i].buf.encoding.short(), "Windows-1252", "guessed");
        app.encoding_action("reopen:koi8-r");
        assert_eq!(app.docs[i].buf.to_string(), "Привет\n");
        assert_eq!(app.docs[i].buf.encoding.short(), "KOI8-R");
        // A reload after a change on disk keeps the encoding the bytes cannot reveal.
        std::fs::write(&f, b"\xF0\xD2\xC9\xD7\xC5\xD4!\n").unwrap();
        app.docs[i].reload_from_disk().unwrap();
        assert_eq!(app.docs[i].buf.to_string(), "Привет!\n");
        // Unsaved edits are never thrown away by a reopen.
        app.docs[i].replace_text("x\n");
        app.reopen_with_encoding(TextEncoding::UTF8);
        assert_eq!(app.docs[i].buf.to_string(), "x\n");
        assert!(status(&app).contains("Unsaved changes"), "{}", status(&app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_files_are_not_opened() {
        let _g = lock();
        let dir = tmp("ff-binary");
        let mut app = test_app(&dir);
        let f = dir.join("blob.bin");
        std::fs::write(&f, b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x03\0>\0\x01\0\0\0").unwrap();
        let n = app.docs.len();
        assert!(app.open_path(&f, None).is_none());
        assert_eq!(app.docs.len(), n);
        assert!(status(&app).contains("binary"), "{}", status(&app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editorconfig_applies_on_open_and_on_save() {
        let _g = lock();
        let dir = tmp("ff-ec");
        let mut app = test_app(&dir);
        std::fs::write(
            dir.join(".editorconfig"),
            "root = true\n[*]\nindent_style = tab\ntab_width = 8\nend_of_line = crlf\ninsert_final_newline = true\ntrim_trailing_whitespace = true\ncharset = utf-8-bom\n[*.md]\ntrim_trailing_whitespace = false\n",
        )
        .unwrap();
        let f = dir.join("a.c");
        std::fs::write(&f, "int x;  ").unwrap();
        let i = app.open_path(&f, None).unwrap();
        app.active = i;
        let d = &app.docs[i];
        assert_eq!((d.ed.indent.as_str(), d.ed.tab_w), ("\t", 8));
        assert!(d.buf.crlf, "no line break yet: the .editorconfig ending");
        assert_eq!(d.buf.encoding, TextEncoding::new(Kind::Utf8, true), "an ASCII file takes the charset");
        app.save_active();
        assert_eq!(std::fs::read(&f).unwrap(), b"\xEF\xBB\xBFint x;\r\n", "trimmed, CRLF final newline, BOM");
        // Markdown: the section turns trimming off, the final newline stays on.
        let md = dir.join("n.md");
        std::fs::write(&md, "hard  \nbreak").unwrap();
        let k = app.open_path(&md, None).unwrap();
        app.active = k;
        app.save_active();
        assert_eq!(std::fs::read(&md).unwrap(), b"\xEF\xBB\xBFhard  \nbreak\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trimming_on_save_is_one_undo_step_and_keeps_cursors() {
        let _g = lock();
        let dir = tmp("ff-trim");
        let mut app = test_app(&dir);
        app.apply_setting("trim_trailing_whitespace", &json!(true));
        app.apply_setting("insert_final_newline", &json!(true));
        let f = dir.join("t.rs");
        std::fs::write(&f, "a   \nb  \nc").unwrap();
        let i = app.open_path(&f, None).unwrap();
        app.active = i;
        {
            let d = &mut app.docs[i];
            // Primary cursor at the end of "c", an extra one after "b" plus one trailing space.
            d.ed.click(&mut d.buf, 10, false);
            d.ed.extra = vec![crate::buffer::Sel::caret(7)];
        }
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nb \nc\n", "the space before the extra cursor stays");
        let d = &mut app.docs[i];
        assert_eq!(d.buf.line_col(d.ed.head()), (2, 1), "still after the c, not on the new last line");
        assert_eq!(d.buf.line_col(d.ed.extra[0].head), (1, 2));
        d.ed.undo(&mut d.buf);
        assert_eq!(d.buf.to_string(), "a   \nb  \nc", "one undo step restores it all");
        // Markdown is never trimmed by the setting alone (two spaces = a line break).
        let md = dir.join("r.md");
        std::fs::write(&md, "line  \nnext\n").unwrap();
        let k = app.open_path(&md, None).unwrap();
        app.active = k;
        app.docs[k].replace_text("line  \nnext \n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "line  \nnext \n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whitespace_toggle_and_rendering() {
        let _g = lock();
        let dir = tmp("ff-ws");
        let mut app = test_app(&dir);
        let f = dir.join("w.txt");
        std::fs::write(&f, "\tindented    with   spaces    \n").unwrap();
        let i = app.open_path(&f, None).unwrap();
        app.active = i;
        app.size = (700, 300);
        // The pixels of the document's text area (status messages and tabs left out).
        let shot = |app: &mut crate::app::App| {
            let (w, h) = (700, 300);
            let mut px = vec![0u32; (w * h) as usize];
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
            let r = app.docs[app.active].rect;
            let mut out = Vec::new();
            for y in r.y.max(0)..(r.y + r.h).min(h) {
                for x in r.x.max(0)..(r.x + r.w).min(w) {
                    out.push(px[(y * w + x) as usize]);
                }
            }
            out
        };
        shot(&mut app);
        let plain = shot(&mut app);
        app.toggle_render_whitespace();
        assert_eq!(app.config.render_whitespace, "all");
        assert_eq!(crate::display::whitespace(), crate::whitespace::Render::All);
        assert!(shot(&mut app) != plain, "the dots and the arrow are drawn");
        app.apply_setting("render_whitespace", &json!("trailing"));
        app.toggle_render_whitespace();
        assert_eq!(app.config.render_whitespace, "none");
        app.toggle_render_whitespace();
        assert_eq!(app.config.render_whitespace, "trailing", "the toggle brings back the last mode");
        app.toggle_render_whitespace();
        assert!(shot(&mut app) == plain, "off again draws exactly what it did before");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
