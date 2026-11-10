use super::*;

// ───────────────────────────── layout ─────────────────────────────

pub struct Pal {
    pub fg: Rgba,
    pub dim: Rgba,
    pub link: Rgba,
    pub code_bg: Rgba,
    pub chip_bg: Rgba,
    pub err: Rgba,
    pub warn: Rgba,
    pub info: Rgba,
    pub rule: Rgba,
}

impl Pal {
    pub fn new(t: &Theme) -> Pal {
        let c = &t.chrome;
        let dark = t.background.luminance() < 0.5;
        let fg = t.foreground;
        Pal {
            fg,
            dim: fg.with_alpha(if dark { 0xb4 } else { 0xa8 }),
            link: c.accent_text,
            code_bg: fg.with_alpha(if dark { 0x12 } else { 0x0d }),
            chip_bg: fg.with_alpha(if dark { 0x1e } else { 0x14 }),
            err: c.error,
            warn: c.warning,
            info: c.info,
            rule: fg.with_alpha(0x26),
        }
    }
    pub(super) fn sev(&self, s: u8) -> Rgba {
        match s {
            1 => self.err,
            2 => self.warn,
            _ => self.info,
        }
    }
}

pub(super) type HlFn<'a> = dyn FnMut(&str, &str) -> Option<Vec<Vec<(usize, usize, Rgba, u8)>>> + 'a;

pub struct Env<'a> {
    pub tx: &'a mut TextCtx,
    pub pal: Pal,
    pub hl: &'a mut HlFn<'a>,
    pub(super) space: i32,
    pub(super) cell: f32,
}

impl<'a> Env<'a> {
    pub fn new(tx: &'a mut TextCtx, pal: Pal, hl: &'a mut HlFn<'a>) -> Env<'a> {
        let space = (tx.ui_width("a a", false) - tx.ui_width("aa", false)).max(3);
        let cell = tx.cell_w;
        Env { tx, pal, hl, space, cell }
    }
    pub(super) fn cells(&self, n: usize) -> i32 {
        (n as f32 * self.cell).round() as i32
    }
}

pub(super) struct Builder {
    pub(super) y: i32,
    pub(super) lines: Vec<LLine>,
    pub(super) decos: Vec<Deco>,
    pub(super) urls: Vec<String>,
    pub(super) links: Vec<(Rect, usize)>,
    pub(super) right: i32,
}

impl Builder {
    pub(super) fn url(&mut self, u: &str) -> usize {
        if let Some(i) = self.urls.iter().position(|x| x == u) {
            return i;
        }
        self.urls.push(u.to_string());
        self.urls.len() - 1
    }
}

#[derive(Clone)]
pub(super) struct Tok {
    pub(super) text: String,
    pub(super) sp: bool,
    pub(super) brk: bool,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) code: bool,
    pub(super) strike: bool,
    pub(super) link: Option<String>,
}

pub(super) fn tokenize(spans: &[Span], force_bold: bool) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut pending = false;
    for s in spans {
        let mk = |text: String, sp: bool, brk: bool| Tok {
            text,
            sp,
            brk,
            bold: s.bold || force_bold,
            italic: s.italic,
            code: s.code,
            strike: s.strike,
            link: s.link.clone(),
        };
        if s.code {
            for (k, part) in s.text.split('\n').enumerate() {
                if k > 0 {
                    out.push(mk(String::new(), false, true));
                    pending = false;
                }
                if !part.is_empty() {
                    out.push(mk(part.to_string(), pending, false));
                    pending = false;
                }
            }
            continue;
        }
        let mut word = String::new();
        let flush = |word: &mut String, pending: &mut bool, out: &mut Vec<Tok>| {
            if !word.is_empty() {
                out.push(mk(std::mem::take(word), *pending, false));
                *pending = false;
            }
        };
        for c in s.text.chars() {
            if c == '\n' {
                flush(&mut word, &mut pending, &mut out);
                out.push(mk(String::new(), false, true));
                pending = false;
            } else if c.is_whitespace() {
                flush(&mut word, &mut pending, &mut out);
                pending = true;
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut pending, &mut out);
    }
    out
}

/// Word-wrap `spans` into lines of at most `w` px starting at `x0`.
pub(super) fn flow(env: &mut Env, b: &mut Builder, spans: &[Span], x0: i32, w: i32, color: Rgba, force_bold: bool) {
    let toks = tokenize(spans, force_bold);
    let w = w.max(40);
    struct P {
        t: Tok,
        w: i32,
    }
    let mut rows: Vec<Vec<P>> = vec![vec![]];
    let mut cur = 0i32;
    let sp = env.space;
    for t in toks {
        if t.brk {
            rows.push(vec![]);
            cur = 0;
            continue;
        }
        let measure = |env: &mut Env, t: &Tok| -> i32 {
            if t.code { env.cells(t.text.chars().count()) + 6 } else { env.tx.ui_width(&t.text, t.bold) }
        };
        let tw = measure(env, &t);
        let row = rows.last_mut().unwrap();
        let lead = if t.sp && !row.is_empty() { sp } else { 0 };
        if tw > w {
            // too long for a line: break it into chunks
            let n = t.text.chars().count().max(1);
            let per = (((w - 6) as f32 / (tw as f32 / n as f32)) as usize).max(4);
            let chars: Vec<char> = t.text.chars().collect();
            let mut first = true;
            for ch in chars.chunks(per) {
                let piece = Tok { text: ch.iter().collect(), sp: first && t.sp, ..t.clone() };
                let pw = measure(env, &piece);
                if !rows.last().unwrap().is_empty() {
                    rows.push(vec![]);
                }
                rows.last_mut().unwrap().push(P { t: piece, w: pw });
                first = false;
            }
            cur = rows.last().unwrap().iter().map(|p| p.w).sum();
            continue;
        }
        if !row.is_empty() && cur + lead + tw > w {
            rows.push(vec![]);
            cur = 0;
            rows.last_mut().unwrap().push(P { t: Tok { sp: false, ..t }, w: tw });
            cur += tw;
        } else {
            cur += lead + tw;
            row.push(P { t, w: tw });
        }
    }
    if rows.last().is_some_and(|r| r.is_empty()) && rows.len() > 1 {
        rows.pop();
    }
    let slant = env.tx.italic_works();
    for row in rows {
        let y = b.y;
        let mut segs: Vec<Seg> = Vec::new();
        let mut plain = String::new();
        let mut x = x0;
        let mut i = 0;
        while i < row.len() {
            let t = &row[i].t;
            if t.sp && i > 0 {
                x += sp;
                plain.push(' ');
            }
            let lk = t.link.clone();
            if t.code {
                let wd = row[i].w;
                let li = lk.as_deref().map(|u| b.url(u));
                b.decos.push(Deco { x, y: y + 1, w: wd, h: PROSE_LH - 2, kind: DecoKind::Fill(env.pal.chip_bg, 3) });
                segs.push(Seg {
                    x: x + 3,
                    w: wd - 6,
                    text: t.text.clone(),
                    color: if li.is_some() { env.pal.link } else { env.pal.fg },
                    bold: false,
                    italic: false,
                    mono: true,
                    strike: t.strike,
                    link: li,
                    size: None,
                });
                if let Some(li) = li {
                    b.links.push((Rect::new(x, y, wd, PROSE_LH), li));
                }
                plain.push_str(&t.text);
                x += wd;
                i += 1;
                continue;
            }
            // merge following plain tokens with the same style
            let mut text = t.text.clone();
            let mut j = i + 1;
            while j < row.len() {
                let n = &row[j].t;
                if n.code || n.bold != t.bold || n.italic != t.italic || n.strike != t.strike || n.link != t.link {
                    break;
                }
                text.push(if n.sp { ' ' } else { '\u{200b}' });
                text.push_str(&n.text);
                j += 1;
            }
            let text = text.replace('\u{200b}', "");
            let wd = env.tx.ui_width(&text, t.bold);
            let li = lk.as_deref().map(|u| b.url(u));
            let c = if li.is_some() {
                env.pal.link
            } else if t.italic && !slant {
                // Fallback only: the slant did not render, so italics are shown a little lighter.
                color.with_alpha((color.3 as u32 * 220 / 255) as u8)
            } else {
                color
            };
            plain.push_str(&text);
            segs.push(Seg {
                x,
                w: wd,
                text,
                color: c,
                bold: t.bold,
                italic: t.italic && slant,
                mono: false,
                strike: t.strike,
                link: li,
                size: None,
            });
            if let Some(li) = li {
                b.links.push((Rect::new(x, y, wd, PROSE_LH), li));
            }
            x += wd;
            i = j;
        }
        b.right = b.right.max(x);
        b.lines.push(LLine { y, h: PROSE_LH, segs, x0: 0, runs: vec![], plain });
        b.y += PROSE_LH;
    }
}

/// One char with its style, for wrapping code.
pub(super) type CharRun = (char, Rgba, u8);

pub(super) fn code_chars(text: &str, spans: Option<&Vec<(usize, usize, Rgba, u8)>>, fg: Rgba) -> Vec<CharRun> {
    let mut out = Vec::with_capacity(text.len());
    let mut k = 0;
    for (bi, ch) in text.char_indices() {
        let mut col = (fg, 0u8);
        if let Some(sp) = spans {
            while k < sp.len() && sp[k].1 <= bi {
                k += 1;
            }
            if let Some(s) = sp.get(k)
                && s.0 <= bi
                && bi < s.1
            {
                col = (s.2, s.3);
            }
        }
        out.push((ch, col.0, col.1));
    }
    out
}

pub(super) fn runs_of(chars: &[CharRun]) -> Vec<(String, Rgba, u8)> {
    let mut out: Vec<(String, Rgba, u8)> = Vec::new();
    for &(ch, c, f) in chars {
        match out.last_mut() {
            Some(l) if l.1 == c && l.2 == f => l.0.push(ch),
            _ => out.push((ch.to_string(), c, f)),
        }
    }
    out
}

/// Wrap a code line at `cols` chars, preferring to break after `,` / before a space.
pub fn wrap_code(chars: Vec<CharRun>, cols: usize) -> Vec<Vec<CharRun>> {
    let cols = cols.max(12);
    if chars.len() <= cols {
        return vec![chars];
    }
    let indent = chars.iter().take_while(|c| c.0 == ' ').count();
    let cont = (indent + 2).min(cols / 3);
    let mut out = Vec::new();
    let mut rest = chars;
    let mut first = true;
    loop {
        let room = if first { cols } else { cols - cont };
        if rest.len() <= room {
            out.push(rest);
            break;
        }
        let lo = room / 2;
        let cut = (lo..room)
            .rev()
            .find(|&i| rest[i - 1].0 == ',' && rest[i].0 == ' ')
            .map(|i| i + 1)
            .or_else(|| (lo..room).rev().find(|&i| rest[i].0 == ' ').map(|i| i + 1))
            .unwrap_or(room);
        let mut tail = rest.split_off(cut.min(rest.len()));
        while rest.last().is_some_and(|c| c.0 == ' ') {
            rest.pop();
        }
        out.push(rest);
        // continuation indent
        let fg = tail.first().map(|c| (c.1, c.2)).unwrap_or((Rgba(0, 0, 0, 255), 0));
        let mut line: Vec<CharRun> = std::iter::repeat_n((' ', fg.0, 0), cont).collect();
        while tail.first().is_some_and(|c| c.0 == ' ') {
            tail.remove(0);
        }
        line.extend(tail);
        rest = line;
        first = false;
    }
    out
}

pub(super) fn code_block(env: &mut Env, b: &mut Builder, text: &str, lang: &str, x0: i32, w: i32, bg: bool) {
    let text = text.replace('\t', "  ");
    let inner_x = x0 + if bg { CODE_PAD } else { 0 };
    let inner_w = w - (inner_x - x0) - if bg { CODE_PAD } else { 0 };
    let cols = (inner_w as f32 / env.cell) as usize;
    let hl = (env.hl)(lang, &text);
    let fg = env.pal.fg;
    let lh = line_h() as i32;
    let top = b.y;
    if bg {
        b.y += 5;
    }
    for (li, line) in text.split('\n').enumerate() {
        let chars = code_chars(line, hl.as_ref().and_then(|h| h.get(li)), fg);
        for part in wrap_code(chars, cols) {
            let plain: String = part.iter().map(|c| c.0).collect();
            b.right = b.right.max(inner_x + env.cells(part.len()) + if bg { CODE_PAD } else { 0 });
            b.lines.push(LLine { y: b.y, h: lh, segs: vec![], x0: inner_x, runs: runs_of(&part), plain });
            b.y += lh;
        }
    }
    if bg {
        b.y += 5;
        b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(env.pal.code_bg, 6) });
    }
}

pub(super) fn label(env: &mut Env, b: &mut Builder, text: &str, x0: i32) {
    // "TYPE ButtonProps" keeps the identifier as written; the fixed titles are all caps
    let up = match text.split_once(' ') {
        Some((first, rest)) if matches!(first, "Type" | "Class") => format!("{} {rest}", first.to_uppercase()),
        _ => text.to_uppercase(),
    };
    let wd = env.tx.ui_width_sized(&up, true, 10.5);
    b.right = b.right.max(x0 + wd);
    b.lines.push(LLine {
        y: b.y,
        h: LABEL_LH,
        segs: vec![Seg {
            x: x0,
            w: wd,
            text: up.clone(),
            color: env.pal.dim,
            bold: true,
            italic: false,
            mono: false,
            strike: false,
            link: None,
            size: Some(10.5),
        }],
        x0: 0,
        runs: vec![],
        plain: text.to_string(),
    });
    b.y += LABEL_LH;
}

pub(super) fn block(env: &mut Env, b: &mut Builder, blk: &Blk, x0: i32, w: i32) {
    let fg = env.pal.fg;
    match blk {
        Blk::Code { text, lang } => code_block(env, b, text, if lang.is_empty() { "ts" } else { lang }, x0, w, true),
        Blk::Para(sp) => flow(env, b, sp, x0, w - x0, fg, false),
        Blk::List(items) => {
            for ListItem { marker, depth, spans } in items {
                let ind = x0 + *depth as i32 * 14;
                let y = b.y;
                let mw = env.tx.ui_width(marker, false);
                b.lines.push(LLine {
                    y,
                    h: PROSE_LH,
                    segs: vec![Seg {
                        x: ind + 12 - mw,
                        w: mw,
                        text: marker.clone(),
                        color: env.pal.dim,
                        bold: false,
                        italic: false,
                        mono: false,
                        strike: false,
                        link: None,
                        size: None,
                    }],
                    x0: 0,
                    runs: vec![],
                    plain: String::new(),
                });
                let n0 = b.lines.len();
                flow(env, b, spans, ind + 16, w - ind - 16 + x0, fg, false);
                // the marker line shares its row with the first text line
                if b.lines.len() > n0 {
                    let first = b.lines.remove(n0);
                    let m = b.lines.remove(n0 - 1);
                    let mut f = first;
                    f.segs.splice(0..0, m.segs);
                    f.plain = format!("{} {}", marker, f.plain);
                    b.lines.insert(n0 - 1, f);
                }
            }
        }
        Blk::Quote(sp) => {
            let top = b.y;
            flow(env, b, sp, x0 + 10, w - x0 - 10, env.pal.dim, false);
            b.decos.push(Deco {
                x: x0,
                y: top + 1,
                w: 3,
                h: b.y - top - 2,
                kind: DecoKind::Fill(env.pal.rule.with_alpha(0x66), 1),
            });
        }
        Blk::Rule => {
            b.y += 3;
            b.decos.push(Deco { x: x0, y: b.y, w: 0, h: 1, kind: DecoKind::Rule(env.pal.rule) });
            b.y += 4;
        }
        Blk::Label(t) => label(env, b, t, x0),
        Blk::Item { head, body, extra } => {
            if let Some(h) = head {
                let cols_w = w - x0;
                let before = b.lines.len();
                code_block_param(env, b, h, x0, cols_w);
                let _ = before;
            }
            let ind = x0 + 14;
            if !body.is_empty() {
                flow(env, b, body, ind, w - ind + x0, env.pal.fg, false);
            }
            for e in extra {
                b.y += 3;
                block(env, b, e, ind, w);
            }
        }
        Blk::Diag { sev, msg, code, url } => {
            let col = env.pal.sev(*sev);
            let top = b.y;
            b.y += 4;
            let mut spans = vec![Span::plain(msg.clone())];
            if !code.is_empty() {
                spans.push(Span::plain(" "));
                spans.push(Span {
                    text: if code.chars().all(|c| c.is_ascii_digit()) { format!("TS{code}") } else { code.clone() },
                    link: url.clone(),
                    ..Default::default()
                });
            }
            flow(env, b, &spans, x0 + 10, w - x0 - 10 - 4, fg, false);
            b.y += 4;
            b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(col.with_alpha(0x1c), 6) });
            b.decos.push(Deco { x: x0, y: top + 3, w: 3, h: b.y - top - 6, kind: DecoKind::Fill(col, 1) });
        }
        Blk::Note { sev, title, body } => {
            let col = env.pal.sev(*sev);
            let top = b.y;
            b.y += 4;
            let mut spans = vec![Span { text: title.clone(), bold: true, ..Default::default() }];
            if !body.is_empty() {
                spans.push(Span::plain(" — "));
                spans.extend(body.iter().cloned());
            }
            flow(env, b, &spans, x0 + 10, w - x0 - 14, fg, false);
            b.y += 4;
            b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(col.with_alpha(0x1c), 6) });
            b.decos.push(Deco { x: x0, y: top + 3, w: 3, h: b.y - top - 6, kind: DecoKind::Fill(col, 1) });
        }
        Blk::Dim(sp) => flow(env, b, sp, x0, w - x0, env.pal.dim, false),
        Blk::Kv { key, val } => {
            let kw = (env.tx.ui_width_sized(key, true, 11.5) + 14).max(60);
            let y = b.y;
            let n0 = b.lines.len();
            flow(env, b, val, x0 + kw, w - x0 - kw, fg, false);
            let kwid = env.tx.ui_width_sized(key, true, 11.5);
            let seg = Seg {
                x: x0,
                w: kwid,
                text: key.clone(),
                color: env.pal.dim,
                bold: true,
                italic: false,
                mono: false,
                strike: false,
                link: None,
                size: Some(11.5),
            };
            if b.lines.len() > n0 {
                b.lines[n0].segs.insert(0, seg);
                b.lines[n0].plain = format!("{key}: {}", b.lines[n0].plain);
            } else {
                b.lines.push(LLine { y, h: PROSE_LH, segs: vec![seg], x0: 0, runs: vec![], plain: key.clone() });
                b.y += PROSE_LH;
            }
        }
    }
}

/// `name: type` of a parameter / return value, highlighted as TypeScript, no background.
pub(super) fn code_block_param(env: &mut Env, b: &mut Builder, head: &str, x0: i32, w: i32) {
    code_block(env, b, head, "ts", x0, w, false);
}

pub fn layout(blocks: &[Blk], w: i32, env: &mut Env) -> Laid {
    let mut b = Builder { y: 0, lines: vec![], decos: vec![], urls: vec![], links: vec![], right: 0 };
    let mut prev_label = false;
    for (i, blk) in blocks.iter().enumerate() {
        if i > 0 {
            let is_item = matches!(blk, Blk::Item { .. });
            b.y += if prev_label {
                3
            } else if matches!(blk, Blk::Label(_)) {
                GAP + 2
            } else if is_item {
                5
            } else {
                GAP
            };
        }
        block(env, &mut b, blk, 0, w);
        prev_label = matches!(blk, Blk::Label(_));
    }
    Laid { w: b.right.min(w), h: b.y, lines: b.lines, decos: b.decos, urls: b.urls, links: b.links }
}

// ───────────────────────────── geometry ─────────────────────────────

/// Popup rectangle for a body of `want_h` px anchored at line `(ax, ay)` (line height `lh`):
/// above the line when it fits (or when there is more room above), else below; always inside
/// `bounds`. `side` is the side chosen before (kept unless it stops fitting and the other is
/// much better), the returned bool is "below".
pub fn place(anchor: (i32, i32), lh: i32, bounds: Rect, w: i32, want_h: i32, side: Option<bool>) -> (Rect, bool) {
    let margin = 4;
    let above = anchor.1 - bounds.y - margin - 3;
    let below_room = bounds.bottom() - (anchor.1 + lh) - margin - 3;
    let h_cap = (bounds.h - 2 * margin).max(60);
    let want = want_h.min(h_cap);
    let below = match side {
        Some(true) if below_room >= want.min(120) || below_room >= above => true,
        Some(false) if above >= want.min(120) || above >= below_room => false,
        _ => {
            if above >= want {
                false
            } else if below_room >= want {
                true
            } else {
                below_room > above
            }
        }
    };
    let avail = if below { below_room } else { above };
    let h = want.min(avail.max(60)).min(h_cap);
    let w = w.min(bounds.w - 2 * margin).max(60);
    let y = if below { anchor.1 + lh + 3 } else { anchor.1 - h - 3 };
    let y = y.clamp(bounds.y + margin, (bounds.bottom() - h - margin).max(bounds.y + margin));
    let x = anchor.0.clamp(bounds.x + margin, (bounds.right() - w - margin).max(bounds.x + margin));
    (Rect::new(x, y, w, h), below)
}
