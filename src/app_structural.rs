//! Structural navigation commands (tree-sitter, no language server): expand the selection by syntax
//! node, jump to the next/previous function or class, select the enclosing function or class.
//! The syntax tree is built when a command runs and dropped right after (see `structural`).

use crate::{
    app::App,
    buffer::Sel,
    structural::{self, Lang, TextObject},
};
use std::path::Path;

impl App {
    /// Parses the active document: its text, a char<->byte map and the tree. Says why when it cannot.
    fn structural_tree(&mut self) -> Option<(String, ropey::Rope, tree_sitter::Tree)> {
        let d = self.docs.get(self.active)?;
        let Some(lang) = Lang::from_path(Path::new(&d.name)) else {
            self.say("structural navigation: no grammar for this file type");
            return None;
        };
        let rope = d.buf.snapshot();
        if rope.len_bytes() > structural::MAX_BYTES {
            self.say("structural navigation: file too large");
            return None;
        }
        let src = rope.to_string();
        let tree = structural::parse(lang, &src);
        if tree.is_none() {
            self.say("structural navigation: could not parse this file");
        }
        tree.map(|t| (src, rope, t))
    }

    fn set_structural_sel(&mut self, rope: &ropey::Rope, r: std::ops::Range<usize>) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let cur = d.ed.sel;
        let (s, e) = (rope.byte_to_char(r.start), rope.byte_to_char(r.end));
        if (s, e) == (cur.start(), cur.end()) {
            return;
        }
        d.sel_stack.push(cur);
        d.ed.sel = Sel { anchor: s, head: e };
        d.reveal = true;
    }

    fn sel_bytes(&self, rope: &ropey::Rope) -> std::ops::Range<usize> {
        let sel = self.docs[self.active].ed.sel;
        rope.char_to_byte(sel.start())..rope.char_to_byte(sel.end())
    }

    /// Expand selection by syntax node (the fallback of `expand_selection` without a language server).
    pub fn structural_expand(&mut self) {
        let Some((src, rope, tree)) = self.structural_tree() else { return };
        let sel = self.sel_bytes(&rope);
        match structural::expand(&tree, &src, sel) {
            Some(r) => self.set_structural_sel(&rope, r),
            None => self.say("selection already covers the whole file"),
        }
    }

    /// Moves the caret to the next/previous function or class.
    pub fn structural_jump(&mut self, obj: TextObject, forward: bool) {
        let Some((_, rope, tree)) = self.structural_tree() else { return };
        let head = rope.char_to_byte(self.docs[self.active].ed.head());
        let Some(at) = structural::jump(&tree, head, obj, forward) else {
            let what = if obj == TextObject::Class { "class" } else { "function" };
            return self.say(format!("no {} {what}", if forward { "next" } else { "previous" }));
        };
        let p = rope.byte_to_char(at);
        let d = &mut self.docs[self.active];
        d.ed.click(&mut d.buf, p, false);
        d.reveal = true;
    }

    /// Selects the function or class around the cursor; again, the next one outward.
    pub fn structural_select(&mut self, obj: TextObject) {
        let Some((src, rope, tree)) = self.structural_tree() else { return };
        let sel = self.sel_bytes(&rope);
        match structural::text_object(&tree, &src, sel, obj, false) {
            Some(r) => self.set_structural_sel(&rope, r),
            None => {
                let what = if obj == TextObject::Class { "class" } else { "function" };
                self.say(format!("not inside a {what}"))
            }
        }
    }
}
