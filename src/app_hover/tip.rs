use super::*;

// ───────────────────────────── composing ─────────────────────────────

impl HoverTip {
    pub(super) fn new(md_text: &str, anchor: (usize, usize), at: (i32, i32)) -> HoverTip {
        let blocks = md::parse_markdown(md_text);
        let mut head = Vec::new();
        let mut rest = Vec::new();
        for b in blocks {
            if rest.is_empty() && matches!(b, Blk::Code { .. }) {
                head.push(b);
            } else {
                rest.push(b);
            }
        }
        let sig = head
            .iter()
            .filter_map(|b| if let Blk::Code { text, .. } = b { Some(text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n");
        let (kind, name) = md::detect_kind(&sig);
        let mut t = HoverTip {
            at,
            anchor,
            serial: 0,
            uri: String::new(),
            ver: 0,
            kind,
            name,
            sig,
            head,
            doc: md::group_jsdoc(rest),
            extra: Extra::default(),
            blocks: vec![],
            blocks_ver: 0,
            laid: None,
            scroll: 0.0,
            scr: Scroller::new(0.3),
            sel: None,
            cells: (None, Default::default()),
            side_below: None,
            rect: Rect::default(),
            view: Rect::default(),
            bar: None,
            hits: vec![],
            has_lsp: !md_text.trim().is_empty(),
        };
        t.compose();
        t
    }

    /// A tip with only diagnostics (no language server answer).
    pub fn diag_only(diags: Vec<DiagInfo>, anchor: (usize, usize), at: (i32, i32)) -> HoverTip {
        let mut t = HoverTip::new("", anchor, at);
        t.extra.diags = diags;
        t.compose();
        t
    }

    /// A tip from Markdown the app made itself (signature help).
    pub fn from_markdown(text: &str, at: (i32, i32), pos: usize) -> HoverTip {
        HoverTip::new(text, (pos, pos), at)
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Plain text for "Copy type".
    pub fn type_text(&self) -> String {
        if let Some(d) = &self.extra.decl
            && matches!(d.kind, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias)
        {
            return d.text.clone();
        }
        self.sig.clone()
    }

    pub fn compose(&mut self) {
        let x = &self.extra;
        let mut out: Vec<Blk> = Vec::new();
        for d in &x.diags {
            out.push(Blk::Diag { sev: d.sev, msg: d.msg.clone(), code: d.code.clone(), url: d.url.clone() });
        }
        let dep = self.doc.tags.iter().find(|t| t.tag == "deprecated");
        if let Some(t) = dep {
            out.push(Blk::Note { sev: 2, title: "Deprecated".into(), body: t.body.clone() });
        }
        // The literal under the pointer, with the type of the property it belongs to.
        let key = x.key_md.as_deref().map(md::parse_markdown).unwrap_or_default();
        let key_doc = md::group_jsdoc(key.iter().filter(|b| !matches!(b, Blk::Code { .. })).cloned().collect());
        for b in key.iter().filter(|b| matches!(b, Blk::Code { .. })) {
            out.push(b.clone());
        }
        out.extend(key_doc.desc);
        if let Some((lit, what)) = &x.literal {
            out.push(Blk::Kv {
                key: "Value".into(),
                val: vec![Span::code(lit.clone()), Span::plain(format!("  {what}"))],
            });
        }

        // Signature: the full declaration when we have it.
        let full =
            x.decl.as_ref().filter(|d| matches!(d.kind, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias));
        if let Some(d) = full {
            out.push(Blk::Code { text: d.text.clone(), lang: "ts".into() });
            if d.more > 0 {
                out.push(Blk::Dim(vec![
                    Span { text: format!("… {} more lines — ", d.more), ..Default::default() },
                    Span { text: "view definition".into(), link: Some("lumen:more".into()), ..Default::default() },
                ]));
            }
        } else {
            for b in &self.head {
                match b {
                    Blk::Code { text, lang } if text.lines().count() > 30 => {
                        let (t, more) = md::truncate_lines(text, 30);
                        out.push(Blk::Code { text: t, lang: lang.clone() });
                        out.push(Blk::Dim(vec![Span::plain(format!("… {more} more lines"))]));
                    }
                    b => out.push(b.clone()),
                }
            }
        }
        if !x.overloads.is_empty() {
            out.push(Blk::Label(format!("Overloads ({})", x.overloads.len())));
            let shown: Vec<&str> = x.overloads.iter().take(8).map(String::as_str).collect();
            out.push(Blk::Code { text: shown.join("\n"), lang: "ts".into() });
            if x.overloads.len() > 8 {
                out.push(Blk::Dim(vec![Span::plain(format!("… and {} more", x.overloads.len() - 8))]));
            }
        }
        if let Some(v) = &x.value
            && !self.sig.trim_end().ends_with(v.as_str())
        {
            out.push(Blk::Kv { key: "Value".into(), val: vec![Span::code(v.clone())] });
        }
        if let Some((code, more)) = &x.snippet {
            out.push(Blk::Label("Declaration".into()));
            out.push(Blk::Code { text: code.clone(), lang: "ts".into() });
            if *more > 0 {
                out.push(Blk::Dim(vec![Span::plain(format!("… {more} more lines"))]));
            }
        }
        if let Some(m) = &x.module {
            out.push(Blk::Kv { key: "Module".into(), val: vec![Span::code(m.spec.clone())] });
            if let Some(f) = &m.file {
                out.push(Blk::Kv {
                    key: "File".into(),
                    val: vec![Span { text: f.clone(), link: x.origin.as_ref().map(origin_link), ..Default::default() }],
                });
            }
            if let Some(p) = &m.pkg {
                out.push(Blk::Kv { key: "Package".into(), val: vec![Span::plain(p.clone())] });
            }
        }

        out.extend(self.doc.desc.iter().cloned());

        // Parameters: the signature's, with the @param descriptions merged in.
        let fnlike = matches!(self.kind, Kind::Function | Kind::Method | Kind::Constructor);
        let sig = if fnlike { md::parse_sig(&self.sig) } else { None };
        let params: Vec<&JsTag> = self
            .doc
            .tags
            .iter()
            .filter(|t| matches!(t.tag.as_str(), "param" | "arg" | "argument" | "prop" | "property"))
            .collect();
        let mut items: Vec<Blk> = Vec::new();
        let mut used = vec![false; params.len()];
        if let Some(s) = &sig {
            for p in &s.params {
                let bare = p.name.trim_start_matches("...");
                let k = params.iter().position(|t| {
                    t.name.as_deref().map(|n| n.trim_start_matches("...").trim_matches(['[', ']'])) == Some(bare)
                });
                let mut body = vec![];
                let mut extra = vec![];
                if let Some(k) = k {
                    used[k] = true;
                    body = params[k].body.clone();
                    extra = params[k].extra.clone();
                }
                let mut head = p.name.clone();
                if p.optional && p.default.is_none() {
                    head.push('?');
                }
                if !p.ty.is_empty() {
                    head.push_str(": ");
                    head.push_str(&p.ty);
                }
                if let Some(d) = &p.default {
                    head.push_str(" = ");
                    head.push_str(d);
                }
                items.push(Blk::Item { head: Some(head), body, extra });
            }
        }
        for (k, t) in params.iter().enumerate() {
            if !used[k] {
                items.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        if !items.is_empty() {
            out.push(Blk::Label("Parameters".into()));
            out.extend(items);
        }
        let tmpl: Vec<&JsTag> =
            self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "template" | "typeParam" | "typeparam")).collect();
        if !tmpl.is_empty() {
            out.push(Blk::Label("Type parameters".into()));
            for t in tmpl {
                out.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        let rets: Vec<&JsTag> =
            self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "returns" | "return")).collect();
        let ret_ty = sig
            .as_ref()
            .and_then(|s| s.ret.clone())
            .filter(|r| (r != "void" || !rets.is_empty()) && self.kind != Kind::Constructor);
        if !rets.is_empty() || ret_ty.is_some() {
            out.push(Blk::Label("Returns".into()));
            if rets.is_empty() {
                out.push(Blk::Item { head: ret_ty.clone(), body: vec![], extra: vec![] });
            }
            for t in rets {
                out.push(Blk::Item { head: ret_ty.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        for t in self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "throws" | "exception")) {
            out.push(Blk::Label("Throws".into()));
            out.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
        }
        let ex: Vec<&JsTag> = self.doc.tags.iter().filter(|t| t.tag == "example").collect();
        if !ex.is_empty() {
            out.push(Blk::Label(if ex.len() > 1 { "Examples".into() } else { "Example".to_string() }));
            for t in ex {
                if !t.body.is_empty() {
                    // The title of an example (`@example Basic usage`) reads as a caption: italic.
                    out.push(Blk::Para(
                        t.body
                            .iter()
                            .cloned()
                            .map(|mut s| {
                                s.italic = true;
                                s
                            })
                            .collect(),
                    ));
                }
                out.extend(t.extra.iter().cloned());
            }
        }
        let see: Vec<&JsTag> = self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "see" | "link")).collect();
        if !see.is_empty() {
            out.push(Blk::Label("See also".into()));
            for t in see {
                out.push(Blk::Para(t.body.clone()));
            }
        }
        for t in self.doc.tags.iter().filter(|t| {
            !matches!(
                t.tag.as_str(),
                "param"
                    | "arg"
                    | "argument"
                    | "prop"
                    | "property"
                    | "template"
                    | "typeParam"
                    | "typeparam"
                    | "returns"
                    | "return"
                    | "throws"
                    | "exception"
                    | "example"
                    | "see"
                    | "link"
                    | "deprecated"
            )
        }) {
            let mut val = t.body.clone();
            if let Some(n) = &t.name {
                val.insert(0, Span::code(n.clone()));
            }
            out.push(Blk::Kv { key: format!("@{}", t.tag), val });
            out.extend(t.extra.iter().cloned());
        }

        // The type of a variable / property / parameter, expanded.
        if let Some((name, d)) = &x.type_decl {
            out.push(Blk::Label(name.clone()));
            out.push(Blk::Code { text: d.text.clone(), lang: "ts".into() });
            if d.more > 0 {
                out.push(Blk::Dim(vec![
                    Span { text: format!("… {} more lines — ", d.more), ..Default::default() },
                    Span {
                        text: "view type definition".into(),
                        link: Some("lumen:more-type".into()),
                        ..Default::default()
                    },
                ]));
            }
        }
        if let Some(o) = &x.origin
            && x.module.is_none()
        {
            out.push(Blk::Kv {
                key: "Defined in".into(),
                val: vec![Span {
                    text: format!("{}:{}", o.label, o.line + 1),
                    link: Some(origin_link(o)),
                    ..Default::default()
                }],
            });
        }
        self.blocks = out;
        self.blocks_ver += 1;
        self.laid = None;
    }

    /// Footer actions that make sense for this tip.
    pub fn actions(&self) -> Vec<Act> {
        let mut v = Vec::new();
        if !self.extra.diags.is_empty() {
            v.push(Act::QuickFix);
        }
        if self.has_lsp {
            v.push(Act::Definition);
            if self.kind.wants_type_def() {
                v.push(Act::TypeDefinition);
            }
            v.push(Act::References);
            if !self.sig.is_empty() {
                v.push(Act::CopyType);
            }
        }
        v
    }
}

pub(super) fn origin_link(o: &Origin) -> String {
    format!("lumen:goto:{}#{},{}", o.path.display(), o.line, o.col)
}
