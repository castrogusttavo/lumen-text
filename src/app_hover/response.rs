use super::*;

impl App {
    /// Diagnostics at char `at` of document `i`.
    pub fn diag_infos_at(&self, i: usize, at: usize) -> Vec<DiagInfo> {
        let d = &self.docs[i];
        let (l, c) = d.buf.line_col(at);
        d.diags
            .iter()
            .filter(|g: &&Diag| {
                (g.sl, g.sc) <= (l, c) && (l, c) < (g.el, g.ec.max(g.sc + 1))
                    || (g.sl == l && g.sc <= c && c <= g.ec.max(g.sc + 1) && g.el == l)
            })
            .map(|g| DiagInfo {
                sev: g.severity,
                msg: g.message.clone(),
                url: (g.source.is_empty() && !g.code.is_empty() && g.code.chars().all(|c| c.is_ascii_digit()))
                    .then(|| format!("https://typescript.tv/errors/#TS{}", g.code)),
                code: match (g.source.is_empty(), g.code.is_empty()) {
                    (true, _) => g.code.clone(),
                    (false, true) => g.source.clone(),
                    (false, false) => format!("{}: {}", g.source, g.code),
                },
                line: g.sl,
                col: g.sc,
            })
            .collect()
    }

    /// Show a tip: replaces the current one, cancels the hide timer, remembers it in the cache.
    pub(super) fn hover_show(&mut self, mut tip: HoverTip) {
        self.hv.serial += 1;
        tip.serial = self.hv.serial;
        self.hv.hide_at = None;
        self.hv.hot = None;
        if tip.has_lsp {
            self.hv.cache.retain(|c| !(c.uri == tip.uri && c.anchor == tip.anchor));
            self.hv.cache.push(tip.clone());
            if self.hv.cache.len() > CACHE_MAX {
                self.hv.cache.remove(0);
            }
        }
        self.hover = Some(tip);
        self.request_redraw();
    }

    /// A tip kept from an earlier hover over the same symbol (same document version).
    pub fn hover_from_cache(&mut self, at: usize) -> bool {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return false };
        let (uri, ver) = (d.uri().unwrap_or_default(), d.version);
        let Some(mut tip) = self
            .hv
            .cache
            .iter()
            .find(|c| c.uri == uri && c.ver == ver && c.anchor.0 <= at && at <= c.anchor.1)
            .cloned()
        else {
            return false;
        };
        let Some(pos) = d.screen_pos(tip.anchor.0) else { return false };
        tip.at = pos;
        tip.scroll = 0.0;
        tip.sel = None;
        tip.side_below = None;
        tip.extra.diags = self.diag_infos_at(i, at);
        tip.compose();
        self.hv.serial += 1;
        tip.serial = self.hv.serial;
        self.hv.hide_at = None;
        self.hover = Some(tip);
        self.request_redraw();
        true
    }

    pub fn hover_show_diags(&mut self, i: usize, at: usize) {
        let diags = self.diag_infos_at(i, at);
        if diags.is_empty() {
            return;
        }
        if let Some(pos) = self.docs[i].screen_pos(at) {
            let mut tip = HoverTip::diag_only(diags, (at, at), pos);
            tip.uri = self.docs[i].uri().unwrap_or_default();
            self.hover_show(tip);
        }
    }

    pub fn hover_set_markdown(&mut self, text: &str) {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return };
        let at = d.ed.head();
        if let Some(pos) = d.screen_pos(at) {
            let mut tip = HoverTip::from_markdown(text, pos, at);
            tip.has_lsp = false;
            tip.uri = d.uri().unwrap_or_default();
            self.hover_show(tip);
        }
    }

    /// The `textDocument/hover` answer.
    pub fn hover_response(&mut self, uri: &str, at: usize, v: &Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        if i != self.active {
            return;
        }
        let md_text = hover_markdown(v);
        let diags = self.diag_infos_at(i, at);
        let d = &self.docs[i];
        let range = v["range"].as_object().map(|_| {
            let r = &v["range"];
            let f = |p: &Value| {
                d.pos_from_lsp(p["line"].as_u64().unwrap_or(0) as usize, p["character"].as_u64().unwrap_or(0) as usize)
            };
            (f(&r["start"]), f(&r["end"]))
        });
        let anchor = range.unwrap_or_else(|| self.word_range(i, at));
        let (ver, uri_s) = (d.version, d.uri().unwrap_or_default());
        if md_text.is_empty() {
            // Nothing from the server: a literal value, or only diagnostics.
            if let Some(lit) = self.literal_hover(i, at, diags.clone()) {
                return lit;
            }
            if diags.is_empty() {
                return;
            }
        }
        let Some(pos) = self.docs[i].screen_pos(anchor.0).or_else(|| self.docs[i].screen_pos(at)) else { return };
        let mut tip = HoverTip::new(&md_text, anchor, pos);
        tip.uri = uri_s;
        tip.ver = ver;
        tip.extra.diags = diags;
        // import / require statements: module and resolved file
        let spec = self.import_at(i, at);
        let is_module = tip.kind == Kind::Module;
        if let Some((s, _, _)) = &spec
            && (is_module || md_text.contains("(alias)") || tip.kind == Kind::Other)
        {
            tip.extra.module = Some(ModInfo { spec: s.clone(), file: None, pkg: None });
        }
        tip.compose();
        self.hover_show(tip);
        if !md_text.is_empty() {
            self.hover_follow_ups(i, anchor.0, spec);
        }
    }

    /// (start, end) char range of the identifier at `at`.
    pub(super) fn word_range(&self, i: usize, at: usize) -> (usize, usize) {
        let d = &self.docs[i];
        let (l, c) = d.buf.line_col(at);
        let line: Vec<char> = d.buf.line(l).chars().collect();
        let (mut s, mut e) = (c.min(line.len()), c.min(line.len()));
        while s > 0 && is_ident(line[s - 1]) {
            s -= 1;
        }
        while e < line.len() && is_ident(line[e]) {
            e += 1;
        }
        (d.buf.pos(l, s), d.buf.pos(l, e))
    }

    /// (specifier, line, char column of the specifier) when `at` is inside an import statement.
    pub(super) fn import_at(&self, i: usize, at: usize) -> Option<(String, usize, usize)> {
        let d = &self.docs[i];
        if !matches!(d.lang, Some("typescript" | "tsx" | "javascript" | "jsx")) {
            return None;
        }
        let (l, _) = d.buf.line_col(at);
        let lo = l.saturating_sub(60);
        let hi = (l + 61).min(d.buf.line_count());
        let lines: Vec<String> = (lo..hi).map(|k| d.buf.line(k)).collect();
        let (s, ln, col) = md::import_spec(&lines, l - lo)?;
        Some((s, ln + lo, col))
    }

    pub(super) fn hover_follow_ups(&mut self, i: usize, anchor: usize, spec: Option<(String, usize, usize)>) {
        let Some(tip) = &self.hover else { return };
        let (serial, kind, uri) = (tip.serial, tip.kind, tip.uri.clone());
        let Some((_, li)) = self.hover_server(i) else { return };
        let ask = |app: &mut App, what: HoverWhat, params: Value, method: &str| {
            app.lsps[li].request(method, params, Pending::HoverExtra { uri: uri.clone(), at: anchor, serial, what });
        };
        if (kind == Kind::Module || (spec.is_some() && self.hover.as_ref().is_some_and(|t| t.extra.module.is_some())))
            && let Some((_, line, col)) = &spec
        {
            let d = &self.docs[i];
            let p = d.buf.pos(*line, *col + 1);
            let params = self.hover_pos_params(i, p);
            ask(self, HoverWhat::Module, params, "textDocument/definition");
        }
        if kind.wants_def() && kind != Kind::Module {
            let params = self.hover_pos_params(i, anchor);
            ask(self, HoverWhat::Def, params, "textDocument/definition");
        }
        if kind.wants_type_def() {
            let params = self.hover_pos_params(i, anchor);
            ask(self, HoverWhat::TypeDef, params, "textDocument/typeDefinition");
        }
    }

    pub(super) fn hover_server(&mut self, i: usize) -> Option<(usize, usize)> {
        let li = self.doc_lsp(i)?;
        self.docs[i].lsp_open.then_some((i, li))
    }

    pub(super) fn hover_pos_params(&self, i: usize, at: usize) -> Value {
        let d = &self.docs[i];
        let (l, c) = d.lsp_pos(at);
        json!({"textDocument": {"uri": d.uri().unwrap_or_default()}, "position": {"line": l, "character": c}})
    }

    /// Text of a file the server pointed at: the open buffer when there is one, else the disk.
    pub(super) fn read_source(&self, uri: &str) -> Option<(String, std::path::PathBuf)> {
        let path = uri_to_path(uri)?;
        if let Some(d) = self.docs.iter().find(|d| d.path() == Some(path.as_path())) {
            return Some((d.buf.to_string(), path));
        }
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > 4_000_000 {
            return None;
        }
        Some((std::fs::read_to_string(&path).ok()?, path))
    }

    /// Answer of a follow-up request (definition, type definition, module resolution, key hover).
    pub fn hover_extra_response(&mut self, uri: &str, at: usize, serial: u64, what: HoverWhat, v: &Value) {
        let live = self.hover.as_ref().is_some_and(|t| t.serial == serial && t.uri == uri && t.anchor.0 == at);
        if !live {
            return;
        }
        let mut changed = false;
        match what {
            HoverWhat::KeyType => {
                let m = hover_markdown(v);
                if !m.is_empty()
                    && let Some(t) = &mut self.hover
                {
                    t.extra.key_md = Some(m);
                    changed = true;
                }
            }
            HoverWhat::Module => {
                if let Some((turi, ..)) = lsp::locations(v).into_iter().next()
                    && let Some(path) = uri_to_path(&turi)
                {
                    let label = self.rel(&path);
                    let pkg = package_of(&path);
                    if let Some(t) = &mut self.hover {
                        if let Some(m) = &mut t.extra.module {
                            m.file = Some(label.clone());
                            m.pkg = pkg;
                        }
                        t.extra.origin = Some(Origin { path, label, line: 0, col: 0 });
                        changed = true;
                    }
                }
            }
            HoverWhat::Def => {
                if let Some((turi, line, col)) = lsp::locations(v).into_iter().next() {
                    changed = self.apply_definition(&turi, line, col);
                }
            }
            HoverWhat::TypeDef => {
                if let Some((turi, line, col)) = lsp::locations(v).into_iter().next() {
                    changed = self.apply_type_definition(&turi, line, col);
                }
            }
        }
        if changed {
            if let Some(t) = &mut self.hover {
                t.compose();
                let c = t.clone();
                if let Some(slot) = self.hv.cache.iter_mut().find(|x| x.serial == serial) {
                    *slot = c;
                }
            }
            self.request_redraw();
        }
    }

    pub(super) fn apply_definition(&mut self, turi: &str, line: usize, col: usize) -> bool {
        let Some((text, path)) = self.read_source(turi) else { return false };
        let Some(off) = byte_off(&text, line, col) else { return false };
        let label = self.rel(&path);
        let Some(t) = &self.hover else { return false };
        let (kind, name) = (t.kind, t.name.clone());
        let at_kind = md::decl_kind_at(&text, off);
        let mut decl = None;
        let mut overloads = Vec::new();
        let mut value = None;
        let mut class_decl = None;
        let mut snippet = None;
        match kind {
            Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias => {
                let k = at_kind
                    .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))
                    .unwrap_or(kind);
                if let Some((text, more)) = md::render_decl(&text, off, k, DECL_LINES) {
                    decl = Some(Decl { text, more, kind: k });
                }
            }
            Kind::Function | Kind::Method => {
                overloads = md::find_overloads(&text, &name, off);
                // `function Button(props: ButtonProps)`: show ButtonProps when it is declared in that file
                let sig = self.hover.as_ref().and_then(|t| md::parse_sig(&t.sig));
                for p in sig.iter().flat_map(|s| s.params.iter()) {
                    let ty = p.ty.trim().trim_end_matches("[]");
                    let base = ty.split('<').next().unwrap_or("");
                    if base.is_empty() || !base.chars().all(is_ident) || !base.starts_with(|c: char| c.is_uppercase()) {
                        continue;
                    }
                    if let Some((o, k)) = find_type_decl(&text, base)
                        && let Some((body, more)) = md::render_decl(&text, o, k, DECL_LINES)
                    {
                        class_decl = Some((format!("Type {base}"), Decl { text: body, more, kind: k }));
                        break;
                    }
                }
            }
            Kind::Constructor if at_kind == Some(Kind::Class) => {
                let cname: String = text[off..].chars().take_while(|c| is_ident(*c)).collect();
                if let Some((body, more)) = md::render_decl(&text, off, Kind::Class, DECL_LINES) {
                    class_decl = Some((format!("Class {cname}"), Decl { text: body, more, kind: Kind::Class }));
                }
            }
            Kind::Variable if at_kind == Some(Kind::Variable) => {
                value = md::short_value(&text, off);
                if value.is_none() && self.hover.as_ref().is_some_and(|t| t.sig.contains("...")) {
                    snippet = md::render_decl(&text, off, Kind::Variable, 14);
                }
            }
            _ => {}
        }
        let is_current = self.docs.get(self.active).and_then(|d| d.path()).is_some_and(|p| p == path);
        let Some(t) = &mut self.hover else { return false };
        let ok =
            decl.is_some() || !overloads.is_empty() || value.is_some() || class_decl.is_some() || snippet.is_some();
        t.extra.snippet = snippet;
        t.extra.type_decl = class_decl.or(t.extra.type_decl.take());
        t.extra.decl = decl;
        t.extra.overloads = overloads;
        t.extra.value = value;
        // `line` is 0-based; shown as line + 1. Not shown for symbols declared right here.
        if !is_current || at_kind.is_none() {
            t.extra.origin = Some(Origin { path, label, line, col });
        }
        ok || t.extra.origin.is_some()
    }

    pub(super) fn apply_type_definition(&mut self, turi: &str, line: usize, col: usize) -> bool {
        let Some((text, path)) = self.read_source(turi) else { return false };
        if is_lib_dts(&path) {
            return false;
        }
        let Some(off) = byte_off(&text, line, col) else { return false };
        let Some(k) = md::decl_kind_at(&text, off)
            .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))
        else {
            return false;
        };
        let name: String = text[off..].chars().take_while(|c| is_ident(*c)).collect();
        let Some(t) = &self.hover else { return false };
        if name.is_empty() || !t.sig.contains(&name) || t.name == name {
            return false;
        }
        let Some((body, more)) = md::render_decl(&text, off, k, DECL_LINES) else { return false };
        let Some(t) = &mut self.hover else { return false };
        t.extra.type_decl = Some((format!("Type {name}"), Decl { text: body, more, kind: k }));
        true
    }

    /// Hover over a literal value: its kind, and the type of the property it is assigned to.
    pub(super) fn literal_hover(&mut self, i: usize, at: usize, diags: Vec<DiagInfo>) -> Option<()> {
        let d = &self.docs[i];
        if !matches!(d.lang, Some("typescript" | "tsx" | "javascript" | "jsx")) {
            return None;
        }
        let (l, c) = d.buf.line_col(at);
        let line = d.buf.line(l);
        let (s, e) = literal_at(&line, c)?;
        let cs: Vec<char> = line.chars().collect();
        let text: String = cs[s..e].iter().collect();
        let (a, b) = (d.buf.pos(l, s), d.buf.pos(l, e));
        let pos = d.screen_pos(a)?;
        // `key: value`: the key sits before a colon
        let mut k = s;
        while k > 0 && cs[k - 1] == ' ' {
            k -= 1;
        }
        let mut key_at = None;
        if k > 0 && (cs[k - 1] == ':' || (cs[k - 1] == '=' && !(k > 1 && matches!(cs[k - 2], '=' | '!' | '<' | '>')))) {
            k -= 1;
            while k > 0 && cs[k - 1] == ' ' {
                k -= 1;
            }
            let end = k;
            if k > 0 && (cs[k - 1] == '"' || cs[k - 1] == '\'') {
                let q = cs[k - 1];
                k -= 1;
                while k > 0 && cs[k - 1] != q {
                    k -= 1;
                }
                k = k.saturating_sub(1);
                if end > k {
                    key_at = Some(d.buf.pos(l, k + 1));
                }
            } else {
                while k > 0 && is_ident(cs[k - 1]) {
                    k -= 1;
                }
                if end > k {
                    key_at = Some(d.buf.pos(l, k));
                }
            }
        }
        let mut tip = HoverTip::new("", (a, b), pos);
        tip.uri = d.uri().unwrap_or_default();
        tip.ver = d.version;
        tip.extra.diags = diags;
        tip.extra.literal = Some((text.clone(), describe_literal(&text)));
        tip.has_lsp = false;
        tip.compose();
        self.hover_show(tip);
        if let Some(ka) = key_at {
            let Some((_, li)) = self.hover_server(i) else { return Some(()) };
            let params = self.hover_pos_params(i, ka);
            let (serial, uri) = self.hover.as_ref().map(|t| (t.serial, t.uri.clone()))?;
            self.lsps[li].request(
                "textDocument/hover",
                params,
                Pending::HoverExtra { uri, at: a, serial, what: HoverWhat::KeyType },
            );
        }
        Some(())
    }
}
