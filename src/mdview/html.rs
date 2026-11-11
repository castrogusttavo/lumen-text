//! The raw HTML subset of the preview: a tolerant tokenizer, inline style tags (`<b> <i> <u> <ins>
//! <mark> <kbd> <sup> <sub> <s> <code> <a> <br> <img>`) inside Markdown paragraphs, and HTML blocks
//! (aligned `<p>` / `<div>` / `<hN>`, `<img>`, `<video>`, simple `<table>` with `colspan` / `align`,
//! `<details>` / `<summary>` around Markdown, `<hr>`, lists as bulleted lines). Comments, `<script>`
//! and `<style>` are dropped; other tags are ignored and their text kept.

use super::*;

/// A start or end tag.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HTag {
    /// Lowercase name.
    pub(super) name: String,
    pub(super) close: bool,
    pub(super) self_close: bool,
    /// The tag source (`<a href="x">`), for its attributes.
    pub(super) raw: String,
}

impl HTag {
    pub(super) fn attr(&self, name: &str) -> Option<String> {
        attr(&self.raw, name).map(|v| decode_entities(&v))
    }

    /// A boolean attribute (`<details open>`), with or without a value.
    pub(super) fn has(&self, name: &str) -> bool {
        let l = self.raw.to_ascii_lowercase();
        let inner = l.trim_start_matches('<').trim_end_matches('>').trim_end_matches('/');
        inner.split(|c: char| c.is_whitespace()).skip(1).any(|t| t == name || t.starts_with(&format!("{name}=")))
    }

    /// `align="center"` (or a `text-align` style) as 0 left / 1 center / 2 right.
    pub(super) fn align(&self) -> u8 {
        let a = self.attr("align").unwrap_or_default().to_ascii_lowercase();
        let style = self.attr("style").unwrap_or_default().to_ascii_lowercase();
        if a == "center" || style.contains("text-align:center") || style.contains("text-align: center") {
            1
        } else if a == "right" || style.contains("text-align:right") || style.contains("text-align: right") {
            2
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Tag(HTag),
    Text(String),
    Comment,
}

/// Split HTML into tags, text and comments. Never fails: a `<` that does not start a tag is text.
pub(super) fn tokens(s: &str) -> Vec<Tok> {
    let mut out = vec![];
    let b = s.as_bytes();
    let (mut i, mut text_from) = (0usize, 0usize);
    let flush = |out: &mut Vec<Tok>, from: usize, to: usize| {
        if to > from {
            out.push(Tok::Text(s[from..to].to_string()));
        }
    };
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if s[i..].starts_with("<!--") {
            flush(&mut out, text_from, i);
            let end = s[i + 4..].find("-->").map_or(s.len(), |e| i + 4 + e + 3);
            out.push(Tok::Comment);
            i = end;
            text_from = i;
            continue;
        }
        let close = b.get(i + 1) == Some(&b'/');
        let ns = i + 1 + close as usize;
        let name_len = b[ns.min(b.len())..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'-').count();
        if name_len == 0 || !b[ns].is_ascii_alphabetic() {
            // `<!DOCTYPE`, `<?xml`, `a < b`: not a tag we know.
            if matches!(b.get(i + 1), Some(b'!' | b'?')) {
                flush(&mut out, text_from, i);
                i = s[i..].find('>').map_or(s.len(), |e| i + e + 1);
                text_from = i;
            } else {
                i += 1;
            }
            continue;
        }
        // Find the closing `>` outside quotes.
        let mut j = ns + name_len;
        let mut q: Option<u8> = None;
        while j < b.len() {
            match (q, b[j]) {
                (Some(c), x) if x == c => q = None,
                (None, b'"' | b'\'') => q = Some(b[j]),
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            i += 1;
            continue;
        }
        flush(&mut out, text_from, i);
        let raw = &s[i..=j];
        out.push(Tok::Tag(HTag {
            name: s[ns..ns + name_len].to_ascii_lowercase(),
            close,
            self_close: raw.ends_with("/>"),
            raw: raw.to_string(),
        }));
        i = j + 1;
        text_from = i;
    }
    flush(&mut out, text_from, b.len());
    out
}

/// The common named entities and numeric references.
pub(super) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';').filter(|e| *e <= 10);
        let rep = end.and_then(|e| {
            let name = &after[..e];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" | "#39" => '\'',
                "nbsp" => '\u{a0}',
                "copy" => '©',
                "reg" => '®',
                "trade" => '™',
                "hellip" => '…',
                "mdash" => '—',
                "ndash" => '–',
                "middot" => '·',
                "times" => '×',
                "rarr" => '→',
                "larr" => '←',
                _ => {
                    let n = name.strip_prefix('#')?;
                    let v = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(v)?
                }
            };
            Some((c, e))
        });
        match rep {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Style flag of an inline tag.
pub(super) fn inline_flag(name: &str) -> Option<Fl> {
    Some(match name {
        "b" | "strong" => F_B,
        "i" | "em" | "cite" | "var" | "dfn" => F_I,
        "s" | "del" | "strike" => F_S,
        "u" | "ins" => F_U,
        "mark" => F_MARK,
        "kbd" => F_KBD,
        "sup" => F_SUP,
        "sub" => F_SUB,
        "code" | "tt" | "samp" => F_CODE,
        _ => return None,
    })
}

fn px_attr(t: &HTag, n: &str) -> Option<i32> {
    t.attr(n)
        .and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f32>().ok())
        .filter(|v| *v >= 1.0)
        .map(|v| v as i32)
}

fn img_of(t: &HTag, link: u32) -> Img {
    let url = t.attr("src").unwrap_or_default();
    Img {
        alt: t.attr("alt").or_else(|| t.attr("title")).unwrap_or_default(),
        video: super::inline::is_video(&url),
        url,
        link,
        size: (px_attr(t, "width"), px_attr(t, "height")),
    }
}

/// Collects the text of an HTML block into paragraphs, images and tables.
struct Render {
    l: (usize, usize),
    spans: Vec<Inl>,
    imgs: Vec<Img>,
    styles: Vec<(String, Fl)>,
    link: u32,
    align: u8,
    head: u8,
    /// Collecting the text of a `<summary>`.
    summary: bool,
    /// `<table>` rows, the cell being filled (its spans live in `spans`).
    table: Option<Vec<Vec<HCell>>>,
    cell: Option<HCell>,
    /// Inside `<video>`: its alt text and poster-less source.
    video: Option<Img>,
    /// Inside `<script>` / `<style>` / `<template>`: drop everything until it closes.
    skip: Option<String>,
}

impl Render {
    fn flags(&self) -> Fl {
        self.styles.iter().fold(if self.link != 0 { F_LINK } else { 0 }, |a, s| a | s.1)
    }

    fn text(&mut self, t: &str) {
        let t = decode_entities(t);
        let mut s = String::with_capacity(t.len());
        let mut sp = false;
        for c in t.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                sp = true;
            } else {
                if sp {
                    s.push(' ');
                }
                sp = false;
                s.push(c);
            }
        }
        if sp {
            s.push(' ');
        }
        let at_start = match self.spans.last() {
            None | Some(Inl::Hard) => true,
            Some(Inl::Text { text, .. }) => text.ends_with(' '),
            _ => false,
        };
        let s = if at_start { s.trim_start().to_string() } else { s };
        if s.is_empty() {
            return;
        }
        let flags = self.flags();
        if let Some(Inl::Text { text, flags: f, link }) = self.spans.last_mut()
            && *f == flags
            && *link == self.link
            && flags & (F_CODE | F_KBD) == 0
        {
            text.push_str(&s);
            return;
        }
        self.spans.push(Inl::Text { text: s, flags, link: self.link });
    }

    fn hard(&mut self) {
        if let Some(Inl::Text { text, .. }) = self.spans.last_mut() {
            let t = text.trim_end().len();
            text.truncate(t);
        }
        if !self.spans.is_empty() {
            self.spans.push(Inl::Hard);
        }
    }

    fn take_spans(&mut self, urls: &mut Urls) -> Vec<Inl> {
        let mut v = std::mem::take(&mut self.spans);
        while matches!(v.last(), Some(Inl::Hard)) {
            v.pop();
        }
        if let Some(Inl::Text { text, .. }) = v.last_mut() {
            let t = text.trim_end().len();
            text.truncate(t);
        }
        super::inline::rewrite(v, urls)
    }

    /// Emit what was collected so far as blocks of the builder's current container.
    fn flush(&mut self, b: &mut Builder) {
        let l = self.l;
        if !self.imgs.is_empty() {
            let imgs = std::mem::take(&mut self.imgs);
            b.push_block(Block { kind: Kind::Images(imgs), line: l.0, end: l.1 });
        }
        if self.cell.is_some() || self.table.is_some() || self.summary {
            return; // the text belongs to a cell / the summary
        }
        let spans = self.take_spans(b.urls);
        if blank(&spans) {
            return;
        }
        let kind = if self.align != 0 {
            Kind::HtmlPara { align: self.align, inl: spans, head: self.head }
        } else if self.head > 0 {
            let slug = slugify(&plain(&spans));
            Kind::Heading { level: self.head, inl: spans, slug }
        } else {
            Kind::Para(spans)
        };
        b.push_block(Block { kind, line: l.0, end: l.1 });
    }

    fn end_cell(&mut self, urls: &mut Urls) {
        if let Some(mut c) = self.cell.take() {
            c.inl = self.take_spans(urls);
            if let Some(t) = self.table.as_mut() {
                if t.is_empty() {
                    t.push(vec![]);
                }
                if let Some(r) = t.last_mut() {
                    r.push(c);
                }
            }
        }
    }
}

impl Builder<'_> {
    /// An image found inside a paragraph (Markdown `![]()` or an inline `<img>`): images end the
    /// text before them and share a row.
    pub(super) fn push_img(&mut self, img: Img) {
        let in_para = self.inl.as_ref().is_some_and(|i| i.kind == IK::Para);
        if in_para {
            if let Some(ic) = self.inl.as_mut() {
                if !ic.spans.is_empty() {
                    let s = std::mem::take(&mut ic.spans);
                    ic.segs.push(Seg::Spans(s));
                }
                ic.segs.push(Seg::Img(img));
            }
        } else {
            let label = if img.alt.is_empty() { "image".to_string() } else { img.alt };
            self.push_span(&format!("[{label}]"), 0);
        }
    }

    /// Close an inline style opened by `^sup^`-like Markdown syntax.
    pub(super) fn pop_style(&mut self, name: &str) {
        if let Some(i) = self.hstack.iter().rposition(|h| h.0 == name) {
            let (_, _, link) = self.hstack.remove(i);
            if link {
                self.links.pop();
            }
        }
    }

    /// Inline styles left open at the end of a paragraph do not leak into the next one.
    pub(super) fn end_inline_html(&mut self) {
        for (_, _, link) in self.hstack.drain(..) {
            if link {
                self.links.pop();
            }
        }
    }

    /// An inline HTML tag inside a paragraph.
    pub(super) fn inline_html(&mut self, raw: &str, range: &std::ops::Range<usize>) {
        for tok in tokens(raw) {
            let Tok::Tag(t) = tok else { continue };
            match (t.name.as_str(), t.close) {
                ("br", _) => {
                    if let Some(ic) = self.inl.as_mut() {
                        ic.spans.push(Inl::Hard);
                    }
                }
                ("img", false) => {
                    if self.inl.is_none() {
                        self.start_inline(IK::Para, range.start, range.end, true);
                    }
                    let link = self.links.last().copied().unwrap_or(0);
                    let img = img_of(&t, link);
                    self.push_img(img);
                }
                (name, true) => self.pop_style(name),
                (_, false) if t.self_close => {}
                ("a", false) => {
                    let href = t.attr("href").unwrap_or_default();
                    let id = if href.is_empty() { 0 } else { self.urls.id(&href) };
                    if id != 0 {
                        self.links.push(id);
                    }
                    self.hstack.push(("a".into(), 0, id != 0));
                }
                (name, false) => {
                    if let Some(f) = inline_flag(name) {
                        self.hstack.push((name.to_string(), f, false));
                    }
                }
            }
        }
    }

    /// A raw HTML block. `<details>` opens a container that the following Markdown blocks go
    /// into, until the block holding `</details>`.
    pub(super) fn html_block(&mut self, raw: &str, l: (usize, usize)) {
        let mut r = Render {
            l,
            spans: vec![],
            imgs: vec![],
            styles: vec![],
            link: 0,
            align: 0,
            head: 0,
            summary: false,
            table: None,
            cell: None,
            video: None,
            skip: None,
        };
        for tok in tokens(raw) {
            if let Some(sk) = r.skip.as_deref() {
                if matches!(&tok, Tok::Tag(t) if t.close && t.name == sk) {
                    r.skip = None;
                }
                continue;
            }
            let t = match tok {
                Tok::Comment => continue,
                Tok::Text(s) => {
                    r.text(&s);
                    continue;
                }
                Tok::Tag(t) => t,
            };
            match (t.name.as_str(), t.close) {
                ("script" | "style" | "template" | "noscript", false) if !t.self_close => r.skip = Some(t.name.clone()),
                ("details", false) => {
                    r.flush(self);
                    let n = self.stack.len() as u64;
                    let id = fxhash(raw.as_bytes(), ((l.0 as u64) << 20) ^ n);
                    self.stack.push(Cont::Details { id, summary: vec![], blocks: vec![], l, open: t.has("open") });
                }
                ("details", true) => {
                    r.flush(self);
                    if let Some(Cont::Details { id, summary, blocks, l: dl, open }) = self.stack.pop_if_details() {
                        let end = l.1.max(dl.1);
                        self.push_block(Block { kind: Kind::Details { id, summary, blocks, open }, line: dl.0, end });
                    }
                }
                ("summary", false) => {
                    r.flush(self);
                    r.summary = true;
                }
                ("summary", true) => {
                    let s = r.take_spans(self.urls);
                    r.summary = false;
                    if let Some(Cont::Details { summary, id, .. }) = self.stack.last_mut() {
                        *id = fxhash(plain(&s).as_bytes(), *id);
                        *summary = s;
                    }
                }
                ("p" | "div" | "center" | "section" | "header" | "footer" | "figure" | "blockquote", false) => {
                    r.flush(self);
                    r.align = if t.name == "center" { 1 } else { t.align() };
                }
                ("p" | "div" | "center" | "section" | "header" | "footer" | "figure" | "blockquote", true) => {
                    r.flush(self);
                    r.align = 0;
                }
                ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                    r.flush(self);
                    r.head = t.name.as_bytes()[1] - b'0';
                    r.align = t.align();
                }
                ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => {
                    r.flush(self);
                    r.head = 0;
                    r.align = 0;
                }
                ("br", _) => r.hard(),
                ("hr", false) => {
                    r.flush(self);
                    self.push_block(Block { kind: Kind::Rule, line: l.0, end: l.1 });
                }
                ("img", false) => {
                    let img = img_of(&t, r.link);
                    if r.cell.is_some() || r.summary {
                        // No images inside cells / summaries: their alt text stands in.
                        if !img.alt.is_empty() {
                            r.text(&img.alt);
                        }
                    } else {
                        r.imgs.push(img);
                    }
                }
                ("video", false) => {
                    let mut v = img_of(&t, r.link);
                    v.video = true;
                    if t.self_close {
                        r.imgs.push(v);
                    } else {
                        r.video = Some(v);
                    }
                }
                ("source", false) => {
                    if let Some(v) = r.video.as_mut()
                        && v.url.is_empty()
                    {
                        v.url = t.attr("src").unwrap_or_default();
                    }
                }
                ("video", true) => {
                    if let Some(v) = r.video.take() {
                        r.imgs.push(v);
                    }
                }
                ("a", false) => {
                    let href = t.attr("href").unwrap_or_default();
                    r.link = if href.is_empty() { 0 } else { self.urls.id(&href) };
                }
                ("a", true) => r.link = 0,
                ("table", false) => {
                    r.flush(self);
                    r.table = Some(vec![]);
                }
                ("tr", false) => {
                    r.end_cell(self.urls);
                    if let Some(tb) = r.table.as_mut() {
                        tb.push(vec![]);
                    }
                }
                ("td" | "th", false) => {
                    r.end_cell(self.urls);
                    if r.table.is_some() {
                        r.spans.clear();
                        let span =
                            t.attr("colspan").and_then(|v| v.trim().parse::<u16>().ok()).unwrap_or(1).clamp(1, 64);
                        r.cell = Some(HCell { inl: vec![], span, align: t.align(), head: t.name == "th" });
                    }
                }
                ("td" | "th", true) => r.end_cell(self.urls),
                ("table", true) => {
                    r.end_cell(self.urls);
                    if let Some(mut rows) = r.table.take() {
                        rows.retain(|row| !row.is_empty());
                        r.spans.clear();
                        if !rows.is_empty() {
                            self.push_block(Block { kind: Kind::HtmlTable(rows), line: l.0, end: l.1 });
                        }
                    }
                }
                ("li", false) => {
                    r.flush(self);
                    r.text("\u{2022} ");
                }
                ("ul" | "ol" | "li" | "dl" | "dt" | "dd", _) => r.flush(self),
                (name, close) => {
                    if let Some(f) = inline_flag(name) {
                        if close {
                            if let Some(i) = r.styles.iter().rposition(|s| s.0 == name) {
                                r.styles.remove(i);
                            }
                        } else if !t.self_close {
                            r.styles.push((name.to_string(), f));
                        }
                    }
                }
            }
        }
        r.end_cell(self.urls);
        r.summary = false;
        if let Some(rows) = r.table.take().filter(|t| !t.is_empty()) {
            self.push_block(Block { kind: Kind::HtmlTable(rows), line: l.0, end: l.1 });
        }
        if let Some(v) = r.video.take() {
            r.imgs.push(v);
        }
        r.flush(self);
    }
}

/// `Vec::pop` that only pops a `<details>` container.
trait PopDetails {
    fn pop_if_details(&mut self) -> Option<Cont>;
}

impl PopDetails for Vec<Cont> {
    fn pop_if_details(&mut self) -> Option<Cont> {
        if matches!(self.last(), Some(Cont::Details { .. })) { self.pop() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_is_tolerant() {
        let t = tokens("a <b>bold</b> <!-- x --> 1 < 2 <a href=\"x>y\">l</a><br/>");
        let tags: Vec<(String, bool)> =
            t.iter().filter_map(|t| if let Tok::Tag(h) = t { Some((h.name.clone(), h.close)) } else { None }).collect();
        assert_eq!(
            tags,
            [("b".into(), false), ("b".into(), true), ("a".into(), false), ("a".into(), true), ("br".into(), false)]
        );
        assert!(t.contains(&Tok::Comment));
        assert!(t.iter().any(|t| matches!(t, Tok::Text(s) if s.contains("1 < 2"))));
        let a =
            t.iter().find_map(|t| if let Tok::Tag(h) = t { (h.name == "a" && !h.close).then_some(h) } else { None });
        assert_eq!(a.and_then(|h| h.attr("href")).as_deref(), Some("x>y"));
        assert!(tokens("<details open>").iter().any(|t| matches!(t, Tok::Tag(h) if h.has("open"))));
        assert!(!tokens("<details>").iter().any(|t| matches!(t, Tok::Tag(h) if h.has("open"))));
        // never panics on garbage
        for s in ["<", "<<", "</", "<a", "<a href='", "&", "&#xZZ;", "&#99999999;", "<!--", "<1>"] {
            let _ = tokens(s);
            let _ = decode_entities(s);
        }
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#233; &#x41; &nbsp;&bogus; &"),
            "a & b <c> é A \u{a0}&bogus; &"
        );
    }
}
