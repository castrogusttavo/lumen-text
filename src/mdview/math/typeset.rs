//! Node tree to positioned glyphs and rules, following TeX's rules (the TeXbook appendix G as
//! adapted by the OpenType MATH specification): atom spacing by class, script styles at the
//! font's 70% / 55%, fractions on the math axis, size variants and assemblies for delimiters,
//! radicals and big operators. Units are logical pixels; y grows downwards from the baseline.

use super::font::{MFont, Part};
use super::parse::{Align, ArrKind, MathErr, Node, R, Style};
use super::syms::Class;
use super::{HBox, Item, TextMeasure};

/// Style plus TeX's cramped flag (superscripts sit lower in cramped styles).
#[derive(Clone, Copy, Debug)]
pub struct St {
    pub s: Style,
    pub cramped: bool,
}

impl St {
    fn with(self, s: Style) -> St {
        St { s, cramped: self.cramped }
    }
    fn script(self) -> Style {
        match self.s {
            Style::D | Style::T => Style::S,
            _ => Style::SS,
        }
    }
    fn sup(self) -> St {
        St { s: self.script(), cramped: self.cramped }
    }
    fn sub(self) -> St {
        St { s: self.script(), cramped: true }
    }
    fn num(self) -> St {
        let s = match self.s {
            Style::D => Style::T,
            Style::T => Style::S,
            _ => Style::SS,
        };
        St { s, cramped: self.cramped }
    }
    fn den(self) -> St {
        St { cramped: true, ..self.num() }
    }
    fn cramp(self) -> St {
        St { cramped: true, ..self }
    }
}

/// A laid-out atom: its box and class, whether it is a single character (scripts attach closer to
/// those) and its glyph (accents use its attachment point).
struct At {
    cls: Class,
    b: HBox,
    ch: bool,
    gid: Option<u16>,
}

impl At {
    fn ord(b: HBox) -> At {
        At { cls: Class::Ord, b, ch: false, gid: None }
    }
}

/// A stretched glyph in font units: pieces `(glyph, x, baseline y up)`, the ink extent along y,
/// the advance width, and the scale applied when no variant was big enough.
struct Built {
    parts: Vec<(u16, f32, f32)>,
    w: f32,
    y0: f32,
    y1: f32,
    sx: f32,
    sy: f32,
}

pub struct Ts<'a, 'm> {
    pub f: &'a MFont,
    /// Font size of the display / text styles, px.
    pub base: f32,
    pub m: &'m mut dyn TextMeasure,
    /// Source length (errors without a position blame the whole formula).
    pub len: usize,
    /// Height `\middle` delimiters stretch to (set while the body of `\left..\right` is redone).
    pub middle: Option<f32>,
}

/// TeX's inter-atom spacing (TeXbook p. 170): 1 thin, 2 medium, 3 thick; negative = not in
/// script styles.
const SPACING: [[i8; 8]; 8] = [
    // Ord Op Bin Rel Open Close Punct Inner
    [0, 1, -2, -3, 0, 0, 0, -1],
    [1, 1, 0, -3, 0, 0, 0, -1],
    [-2, -2, 0, 0, -2, 0, 0, -2],
    [-3, -3, 0, 0, -3, 0, 0, -3],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, -2, -3, 0, 0, 0, -1],
    [-1, -1, 0, -1, -1, -1, -1, -1],
    [-1, 1, -2, -3, -1, 0, -1, -1],
];

fn class_ix(c: Class) -> usize {
    match c {
        Class::Ord => 0,
        Class::Op => 1,
        Class::Bin => 2,
        Class::Rel => 3,
        Class::Open => 4,
        Class::Close => 5,
        Class::Punct => 6,
        Class::Inner => 7,
    }
}

impl Ts<'_, '_> {
    fn size(&self, st: St) -> f32 {
        self.base
            * match st.s {
                Style::D | Style::T => 1.0,
                Style::S => self.f.c.sp,
                Style::SS => self.f.c.ssp,
            }
    }

    /// Pixels per font unit in a style.
    fn k(&self, st: St) -> f32 {
        self.size(st) / self.f.upem
    }

    fn glyph(&self, c: char) -> R<u16> {
        self.f.glyph(c).ok_or_else(|| MathErr { at: (0, self.len.max(1)), msg: format!("the math font has no {c}") })
    }

    fn glyph_box(&self, g: u16, k: f32) -> HBox {
        let (_, y0, _, y1) = self.f.bbox(g);
        HBox {
            w: self.f.adv(g) * k,
            h: (y1 * k).max(0.0),
            d: (-y0 * k).max(0.0),
            items: vec![Item::Glyph { x: 0.0, y: 0.0, gid: g, sx: k, sy: k }],
            ic: self.f.italic(g) * k,
        }
    }

    fn space(&self, l: Class, r: Class, st: St) -> f32 {
        let v = SPACING[class_ix(l)][class_ix(r)];
        if v < 0 && matches!(st.s, Style::S | Style::SS) {
            return 0.0;
        }
        let mu = match v.abs() {
            1 => 3.0,
            2 => 4.0,
            3 => 5.0,
            _ => 0.0,
        };
        mu / 18.0 * self.size(st)
    }

    /// A list of atoms, spaced by class.
    pub fn list(&mut self, ns: &[Node], st0: St) -> R<HBox> {
        let mut st = st0;
        let mut atoms: Vec<(Option<Class>, HBox, St)> = Vec::with_capacity(ns.len());
        for n in ns {
            match n {
                Node::Style(s) => st = st.with(*s),
                Node::Space(em) => atoms.push((None, HBox { w: em * self.size(st), ..HBox::default() }, st)),
                _ => {
                    let a = self.node(n, st)?;
                    atoms.push((Some(a.cls), a.b, st));
                }
            }
        }
        // A binary operator with nothing to combine becomes ordinary (`-x`, `(+`, `= -`).
        let mut prev: Option<usize> = None;
        for i in 0..atoms.len() {
            let Some(c) = atoms[i].0 else { continue };
            if c == Class::Bin {
                let pc = prev.and_then(|p| atoms[p].0);
                if matches!(pc, None | Some(Class::Bin | Class::Op | Class::Rel | Class::Open | Class::Punct)) {
                    atoms[i].0 = Some(Class::Ord);
                }
            } else if matches!(c, Class::Rel | Class::Close | Class::Punct)
                && let Some(p) = prev
                && atoms[p].0 == Some(Class::Bin)
            {
                atoms[p].0 = Some(Class::Ord);
            }
            prev = Some(i);
        }
        if let Some(p) = prev
            && atoms[p].0 == Some(Class::Bin)
        {
            atoms[p].0 = Some(Class::Ord);
        }
        let mut out = HBox::default();
        let mut last: Option<Class> = None;
        for (c, b, s) in atoms {
            if let Some(c) = c {
                if let Some(l) = last {
                    out.w += self.space(l, c, s);
                }
                last = Some(c);
            }
            out.append(b);
        }
        Ok(out)
    }

    fn node(&mut self, n: &Node, st: St) -> R<At> {
        let k = self.k(st);
        Ok(match n {
            Node::Char { c, class, at } => {
                let g = self.f.glyph(*c).ok_or_else(|| MathErr {
                    at: (at.0 as usize, at.1 as usize),
                    msg: format!("the math font has no {c}"),
                })?;
                At { cls: *class, b: self.glyph_box(g, k), ch: true, gid: Some(g) }
            }
            Node::Text { s, bold } => {
                let size = self.size(st);
                let m = self.m.text(s, size, *bold);
                At::ord(HBox {
                    w: m.w,
                    h: m.asc,
                    d: m.desc,
                    items: vec![Item::Text { x: 0.0, y: 0.0, s: s.clone(), size, bold: *bold, bl: m.bl }],
                    ic: 0.0,
                })
            }
            Node::Word { s, .. } => {
                let mut b = HBox::default();
                for c in s.chars() {
                    if let Some(g) = self.f.glyph(c) {
                        b.append(self.glyph_box(g, k));
                    } else if c == ' ' {
                        b.w += 0.25 * self.size(st);
                    }
                }
                b.ic = 0.0;
                At { cls: Class::Op, b, ch: false, gid: None }
            }
            Node::BigOp { c, at, .. } => self.big_op(*c, *at, st)?,
            Node::Group(l) => At::ord(self.list(l, st)?),
            Node::Class(c, l) => At { cls: *c, ..At::ord(self.list(l, st)?) },
            Node::Scripts { base, sup, sub } => self.scripts(base, sup.as_deref(), sub.as_deref(), st)?,
            Node::Frac { num, den, rule, style, delims } => self.frac(num, den, *rule, *style, *delims, st)?,
            Node::Sqrt { body, index } => self.sqrt(body, index.as_deref(), st)?,
            Node::LeftRight { left, right, body } => self.left_right(*left, *right, body, st)?,
            Node::Middle(c) => {
                let t = self.middle.unwrap_or(0.0);
                At::ord(self.delim_box(*c, t, st)?)
            }
            Node::Big { c, size, class } => {
                let em = self.size(st);
                let t = [1.2, 1.8, 2.4, 3.0][(*size).min(3) as usize] * em;
                At { cls: *class, ..At::ord(self.delim_box(*c, t, st)?) }
            }
            Node::Accent { c, body, wide } => self.accent(*c, body, *wide, st)?,
            Node::Overline(l) => {
                let c = &self.f.c;
                let (gap, t, extra) = (c.overbar_gap * k, c.overbar_rule * k, c.overbar_extra * k);
                let b = self.list(l, st.cramp())?;
                let (bw, bh) = (b.w, b.h);
                let mut out = HBox::default();
                out.add(b, 0.0, 0.0);
                out.items.push(Item::Rule { x: 0.0, y: -(bh + gap + t), w: bw, h: t });
                out.h = bh + gap + t + extra;
                out.w = bw;
                At::ord(out)
            }
            Node::Underline(l) => {
                let c = &self.f.c;
                let (gap, t, extra) = (c.underbar_gap * k, c.underbar_rule * k, c.underbar_extra * k);
                let b = self.list(l, st)?;
                let (bw, bd) = (b.w, b.d);
                let mut out = HBox::default();
                out.add(b, 0.0, 0.0);
                out.items.push(Item::Rule { x: 0.0, y: bd + gap, w: bw, h: t });
                out.d = bd + gap + t + extra;
                out.w = bw;
                At::ord(out)
            }
            Node::Brace { body, over } => self.brace(body, *over, st)?,
            Node::Stack { base, over, under, class } => {
                let b = At::ord(self.list(base, st)?);
                let mut r = self.limits(b, over.as_deref(), under.as_deref(), st, false)?;
                r.cls = *class;
                r
            }
            Node::XArrow { c, over, under } => self.xarrow(*c, over, under.as_deref(), st)?,
            Node::Space(em) => At::ord(HBox { w: em * self.size(st), ..HBox::default() }),
            Node::Style(_) => At::ord(HBox::default()),
            Node::Array { rows, cols, kind } => self.array(rows, cols, *kind, st)?,
            Node::Phantom { body, w, h } => {
                let b = self.list(body, st)?;
                At::ord(HBox {
                    w: if *w { b.w } else { 0.0 },
                    h: if *h { b.h } else { 0.0 },
                    d: if *h { b.d } else { 0.0 },
                    items: vec![],
                    ic: 0.0,
                })
            }
            Node::Boxed(l) => {
                let b = self.list(l, st)?;
                let pad = 0.3 * self.size(st);
                let t = (self.f.c.frac_rule * k).max(0.5);
                let (bw, bh, bd) = (b.w, b.h, b.d);
                let (w, top, bot) = (bw + 2.0 * (pad + t), bh + pad + t, bd + pad + t);
                let mut out = HBox::default();
                out.add(b, pad + t, 0.0);
                out.items.push(Item::Rule { x: 0.0, y: -top, w, h: t });
                out.items.push(Item::Rule { x: 0.0, y: bot - t, w, h: t });
                out.items.push(Item::Rule { x: 0.0, y: -top, w: t, h: top + bot });
                out.items.push(Item::Rule { x: w - t, y: -top, w: t, h: top + bot });
                out.w = w;
                out.h = top;
                out.d = bot;
                At::ord(out)
            }
            Node::Not(inner) => {
                let a = self.node(inner, st)?;
                let g = self.glyph('\u{338}')?;
                let (x0, _, x1, _) = self.f.bbox(g);
                let mut b = a.b;
                let dx = b.w / 2.0 - (x0 + x1) / 2.0 * k;
                b.items.push(Item::Glyph { x: dx, y: 0.0, gid: g, sx: k, sy: k });
                let (_, gy0, _, gy1) = self.f.bbox(g);
                b.h = b.h.max(gy1 * k);
                b.d = b.d.max(-gy0 * k);
                At { cls: a.cls, b, ch: false, gid: None }
            }
        })
    }

    /// Large operator: the display size variant in display style, centered on the math axis.
    fn big_op(&mut self, c: char, at: (u32, u32), st: St) -> R<At> {
        let f = self.f;
        let k = self.k(st);
        let mut g = f
            .glyph(c)
            .ok_or_else(|| MathErr { at: (at.0 as usize, at.1 as usize), msg: format!("the math font has no {c}") })?;
        if st.s == Style::D {
            let vs = f.variants(g, true);
            if let Some(&(vg, _)) = vs.iter().find(|v| v.1 >= f.c.display_op_min).or(vs.last()) {
                g = vg;
            }
        }
        let (_, y0, _, y1) = f.bbox(g);
        let up = f.c.axis - (y0 + y1) / 2.0;
        let b = HBox {
            w: f.adv(g) * k,
            h: ((y1 + up) * k).max(0.0),
            d: (-(y0 + up) * k).max(0.0),
            items: vec![Item::Glyph { x: 0.0, y: -up * k, gid: g, sx: k, sy: k }],
            ic: f.italic(g) * k,
        };
        Ok(At { cls: Class::Op, b, ch: false, gid: None })
    }

    fn scripts(&mut self, base: &Node, sup: Option<&[Node]>, sub: Option<&[Node]>, st: St) -> R<At> {
        let limits = match base {
            Node::BigOp { limits, integral, .. } => limits.unwrap_or(!integral && st.s == Style::D),
            Node::Word { limits, dl, .. } => limits.unwrap_or(*dl && st.s == Style::D),
            Node::Class(Class::Op, _) => st.s == Style::D,
            Node::Brace { .. } => true,
            _ => false,
        };
        let is_op = matches!(base, Node::BigOp { .. });
        let at = self.node(base, st)?;
        if limits {
            return self.limits(at, sup, sub, st, true);
        }
        let f = self.f;
        let c = &f.c;
        let k = self.k(st);
        let sup_b = match sup {
            Some(l) => Some(self.list(l, st.sup())?),
            None => None,
        };
        let sub_b = match sub {
            Some(l) => Some(self.list(l, st.sub())?),
            None => None,
        };
        let (bw, bh, bd) = (at.b.w, at.b.h, at.b.d);
        let ic = if at.ch || is_op { at.b.ic } else { 0.0 };
        let (mut u, mut v) = (0.0f32, 0.0f32);
        if let Some(sb) = &sup_b {
            u = if at.ch { 0.0 } else { bh - c.sup_drop_max * k };
            u = u.max(if st.cramped { c.sup_up_cramped } else { c.sup_up } * k).max(sb.d + c.sup_bottom_min * k);
        }
        if let Some(bb) = &sub_b {
            v = if at.ch { 0.0 } else { bd + c.sub_drop_min * k };
            v = v.max(c.sub_down * k).max(bb.h - c.sub_top_max * k);
        }
        if let (Some(sb), Some(bb)) = (&sup_b, &sub_b) {
            let gap = (u - sb.d) - (bb.h - v);
            let min = c.subsup_gap_min * k;
            if gap < min {
                v += min - gap;
                let p = c.sup_bottom_max_with_sub * k - (u - sb.d);
                if p > 0.0 {
                    u += p;
                    v -= p;
                }
            }
        }
        let mut out = HBox::default();
        out.add(at.b, 0.0, 0.0);
        let mut right = bw;
        if let Some(sb) = sup_b {
            let x = bw + ic;
            right = right.max(x + sb.w);
            out.add(sb, x, -u);
        }
        if let Some(bb) = sub_b {
            let x = bw - if is_op { ic } else { 0.0 };
            right = right.max(x + bb.w);
            out.add(bb, x, v);
        }
        out.w = right + c.space_after_script * k;
        Ok(At { cls: at.cls, b: out, ch: false, gid: None })
    }

    /// Limits above / below (`big`: a large operator, with the font's limit gaps; otherwise a
    /// stack such as `\overset`, with a small fixed gap).
    fn limits(&mut self, at: At, sup: Option<&[Node]>, sub: Option<&[Node]>, st: St, big: bool) -> R<At> {
        let f = self.f;
        let c = &f.c;
        let k = self.k(st);
        let em = self.size(st);
        let ub = match sup {
            Some(l) => Some(self.list(l, st.sup())?),
            None => None,
        };
        let lb = match sub {
            Some(l) => Some(self.list(l, st.sub())?),
            None => None,
        };
        let b = at.b;
        let w = b.w.max(ub.as_ref().map_or(0.0, |u| u.w)).max(lb.as_ref().map_or(0.0, |l| l.w));
        let (bh, bd, bw, ic) = (b.h, b.d, b.w, if big { b.ic } else { 0.0 });
        let mut out = HBox::default();
        out.add(b, (w - bw) / 2.0, 0.0);
        if let Some(u) = ub {
            let shift =
                if big { bh + (c.upper_gap_min * k + u.d).max(c.upper_rise_min * k) } else { bh + 0.12 * em + u.d };
            let x = (w - u.w) / 2.0 + ic / 2.0;
            out.add(u, x, -shift);
        }
        if let Some(l) = lb {
            let shift =
                if big { bd + (c.lower_gap_min * k + l.h).max(c.lower_drop_min * k) } else { bd + 0.12 * em + l.h };
            let x = (w - l.w) / 2.0 - ic / 2.0;
            out.add(l, x, shift);
        }
        out.w = w;
        Ok(At { cls: at.cls, b: out, ch: false, gid: None })
    }

    fn frac(
        &mut self,
        num: &[Node],
        den: &[Node],
        rule: bool,
        style: Option<Style>,
        delims: Option<(char, char)>,
        st: St,
    ) -> R<At> {
        let st = style.map_or(st, |s| st.with(s));
        let f = self.f;
        let c = &f.c;
        let k = self.k(st);
        let nb = self.list(num, st.num())?;
        let db = self.list(den, st.den())?;
        let disp = st.s == Style::D;
        let a = c.axis * k;
        let t = if rule { c.frac_rule * k } else { 0.0 };
        let pick = |d: f32, t: f32| if disp { d } else { t };
        let (mut u, mut v) = if rule {
            (pick(c.num_up_d, c.num_up) * k, pick(c.den_down_d, c.den_down) * k)
        } else {
            (pick(c.stack_up_d, c.stack_up) * k, pick(c.stack_down_d, c.stack_down) * k)
        };
        if rule {
            let gn = pick(c.num_gap_d, c.num_gap) * k;
            let gd = pick(c.den_gap_d, c.den_gap) * k;
            let d1 = (u - nb.d) - (a + t / 2.0);
            if d1 < gn {
                u += gn - d1;
            }
            let d2 = (a - t / 2.0) - (db.h - v);
            if d2 < gd {
                v += gd - d2;
            }
        } else {
            let g = pick(c.stack_gap_d, c.stack_gap) * k;
            let gap = (u - nb.d) - (db.h - v);
            if gap < g {
                u += (g - gap) / 2.0;
                v += (g - gap) / 2.0;
            }
        }
        let pad = 0.12 * self.size(st);
        let w = nb.w.max(db.w);
        let (nw, dw) = (nb.w, db.w);
        let mut out = HBox::default();
        out.add(nb, pad + (w - nw) / 2.0, -u);
        out.add(db, pad + (w - dw) / 2.0, v);
        if rule {
            out.items.push(Item::Rule { x: pad, y: -(a + t / 2.0), w, h: t });
            out.h = out.h.max(a + t / 2.0);
        }
        out.w = w + 2.0 * pad;
        if let Some((l, r)) = delims {
            out = self.wrap(out, Some(l), Some(r), st)?;
        }
        Ok(At::ord(out))
    }

    /// `body` between delimiters stretched to cover it (TeX's \delimiterfactor 901 and
    /// \delimitershortfall 5pt).
    fn wrap(&mut self, body: HBox, l: Option<char>, r: Option<char>, st: St) -> R<HBox> {
        let t = self.cover(&body, st);
        let lb = self.delim_box(l, t, st)?;
        let rb = self.delim_box(r, t, st)?;
        let mut out = HBox::default();
        out.append(lb);
        out.append(body);
        out.append(rb);
        Ok(out)
    }

    /// Height a delimiter needs to cover `b`, centered on the axis.
    fn cover(&self, b: &HBox, st: St) -> f32 {
        let a = self.f.c.axis * self.k(st);
        let m = (b.h - a).max(b.d + a);
        (2.0 * m * 0.901).max(2.0 * m - 0.5 * self.size(st))
    }

    fn left_right(&mut self, l: Option<char>, r: Option<char>, body: &[Node], st: St) -> R<At> {
        let saved = self.middle.take();
        let mut b = self.list(body, st)?;
        let t = self.cover(&b, st);
        if body.iter().any(|n| matches!(n, Node::Middle(_))) {
            self.middle = Some(t);
            b = self.list(body, st)?;
        }
        self.middle = saved;
        let lb = self.delim_box(l, t, st)?;
        let rb = self.delim_box(r, t, st)?;
        let mut out = HBox::default();
        out.append(lb);
        out.append(b);
        out.append(rb);
        Ok(At { cls: Class::Inner, b: out, ch: false, gid: None })
    }

    /// A delimiter at least `target` px tall, centered on the axis (`None`: the null delimiter).
    fn delim_box(&mut self, c: Option<char>, target: f32, st: St) -> R<HBox> {
        let k = self.k(st);
        let Some(ch) = c else { return Ok(HBox { w: 0.12 * self.size(st), ..HBox::default() }) };
        let g = self.glyph(ch)?;
        let b = self.vbuild(g, target / k);
        let up = self.f.c.axis - (b.y0 + b.y1) / 2.0;
        Ok(HBox {
            w: b.w * k,
            h: ((b.y1 + up) * k).max(0.0),
            d: (-(b.y0 + up) * k).max(0.0),
            items: built_items(&b, k, 0.0, up),
            ic: 0.0,
        })
    }

    /// `g` grown vertically to `tu` font units: the first size variant that is tall enough, else
    /// its assembly, else the largest variant scaled.
    fn vbuild(&self, g: u16, tu: f32) -> Built {
        let f = self.f;
        let single = |g: u16| {
            let (_, y0, _, y1) = f.bbox(g);
            Built { parts: vec![(g, 0.0, 0.0)], w: f.adv(g), y0, y1, sx: 1.0, sy: 1.0 }
        };
        let vs = f.variants(g, true);
        if let Some(&(vg, _)) = vs.iter().find(|v| v.1 >= tu) {
            return single(vg);
        }
        let (_, gy0, _, gy1) = f.bbox(g);
        if vs.is_empty() && gy1 - gy0 >= tu {
            return single(g);
        }
        if let Some(parts) = f.assembly(g, true) {
            return self.assemble(&parts, tu, true);
        }
        let mut b = single(vs.last().map_or(g, |v| v.0));
        let h = b.y1 - b.y0;
        if h > 0.0 && tu > h {
            b.sy = tu / h;
            b.y0 *= b.sy;
            b.y1 *= b.sy;
        }
        b
    }

    /// `g` grown horizontally to `tw` font units (wide accents, braces, extensible arrows).
    fn hbuild(&self, g: u16, tw: f32) -> Built {
        let f = self.f;
        let single = |g: u16| {
            let (_, y0, _, y1) = f.bbox(g);
            Built { parts: vec![(g, 0.0, 0.0)], w: f.adv(g), y0, y1, sx: 1.0, sy: 1.0 }
        };
        let vs = f.variants(g, false);
        if let Some(&(vg, _)) = vs.iter().find(|v| v.1 >= tw) {
            return single(vg);
        }
        if let Some(parts) = f.assembly(g, false) {
            return self.assemble(&parts, tw, false);
        }
        let mut b = single(vs.last().map_or(g, |v| v.0));
        let (x0, _, x1, _) = f.bbox(b.parts[0].0);
        let w = if b.w > 0.0 { b.w } else { x1 - x0 };
        if w > 0.0 && tw > w {
            b.sx = tw / w;
            b.w *= b.sx;
        }
        b
    }

    /// Pieces of an assembly repeated until they span `t` font units (with the minimum connector
    /// overlap between neighbours).
    fn assemble(&self, parts: &[Part], t: f32, vertical: bool) -> Built {
        let f = self.f;
        let o = f.min_overlap;
        let n_ext = parts.iter().filter(|p| p.ext).count();
        let mut reps = if n_ext > 0 { 1usize } else { 0 };
        loop {
            let count = parts.len() - n_ext + n_ext * reps;
            let total: f32 = parts.iter().map(|p| if p.ext { p.adv * reps as f32 } else { p.adv }).sum::<f32>()
                - o * count.saturating_sub(1) as f32;
            if total >= t || n_ext == 0 || reps >= 64 {
                break;
            }
            reps += 1;
        }
        let mut out = Built { parts: vec![], w: 0.0, y0: f32::MAX, y1: f32::MIN, sx: 1.0, sy: 1.0 };
        let mut pos = 0.0f32;
        let mut first = true;
        for p in parts {
            let n = if p.ext { reps } else { 1 };
            for _ in 0..n {
                if !first {
                    pos -= o;
                }
                first = false;
                let (gx0, gy0, _, gy1) = f.bbox(p.g);
                if vertical {
                    // the piece's ink bottom sits at `pos`
                    out.parts.push((p.g, 0.0, pos - gy0));
                    out.w = out.w.max(f.adv(p.g));
                } else {
                    out.parts.push((p.g, pos - gx0.min(0.0), 0.0));
                    out.y0 = out.y0.min(gy0);
                    out.y1 = out.y1.max(gy1);
                }
                pos += p.adv;
            }
        }
        if vertical {
            out.y0 = 0.0;
            out.y1 = pos;
        } else {
            out.w = pos;
            if out.y0 > out.y1 {
                out.y0 = 0.0;
                out.y1 = 0.0;
            }
        }
        out
    }

    fn sqrt(&mut self, body: &[Node], index: Option<&[Node]>, st: St) -> R<At> {
        let f = self.f;
        let c = &f.c;
        let k = self.k(st);
        let b = self.list(body, st.cramp())?;
        let t = c.rad_rule * k;
        let gap = if st.s == Style::D { c.rad_gap_d } else { c.rad_gap } * k;
        let need = b.h + b.d + gap + t;
        let g = self.glyph('√')?;
        let built = self.vbuild(g, need / k);
        let ink = (built.y1 - built.y0) * k;
        let extra = ((ink - need) / 2.0).max(0.0);
        let top = b.h + gap + extra + t;
        // move the radical so that its ink top is at `top`
        let up = top / k - built.y1;
        let gw = built.w * k;
        let bottom = (built.y0 + up) * k;
        let (bw, bd) = (b.w, b.d);
        let mut rad = HBox { items: built_items(&built, k, 0.0, up), ..HBox::default() };
        rad.add(b, gw, 0.0);
        rad.items.push(Item::Rule { x: gw, y: -top, w: bw, h: t });
        rad.w = gw + bw;
        rad.h = top + c.rad_extra * k;
        rad.d = bd.max(-bottom);
        let Some(index) = index else { return Ok(At::ord(rad)) };
        let ib = self.list(index, St { s: Style::SS, cramped: true })?;
        let kb = c.rad_kern_before * k;
        let ka = c.rad_kern_after * k;
        let raise = bottom + c.rad_raise * ink;
        // the degree sits over the radical's left arm (the kern after it is negative)
        let rx = kb + ib.w + ka;
        let shift = (-rx).max(-kb).max(0.0);
        let rw = rad.w;
        let mut out = HBox::default();
        out.add(ib, kb + shift, -raise);
        out.add(rad, rx + shift, 0.0);
        out.w = rx + shift + rw;
        Ok(At::ord(out))
    }

    fn accent(&mut self, c: char, body: &[Node], wide: bool, st: St) -> R<At> {
        let f = self.f;
        let k = self.k(st);
        let at = if body.len() == 1 { self.node(&body[0], st.cramp())? } else { At::ord(self.list(body, st.cramp())?) };
        let single = at.ch;
        let b = at.b;
        let g0 = self.glyph(c)?;
        let built = if wide {
            self.hbuild(g0, b.w / k)
        } else {
            let (_, y0, _, y1) = f.bbox(g0);
            Built { parts: vec![(g0, 0.0, 0.0)], w: f.adv(g0), y0, y1, sx: 1.0, sy: 1.0 }
        };
        // horizontal attachment of the accent: its top-accent point, else its ink center
        let att_acc = match built.parts.as_slice() {
            [(g, _, _)] if (built.sx - 1.0).abs() < 1e-6 => f.top_accent(*g).unwrap_or_else(|| {
                let (x0, _, x1, _) = f.bbox(*g);
                (x0 + x1) / 2.0
            }),
            parts => {
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for (g, x, _) in parts {
                    let (x0, _, x1, _) = f.bbox(*g);
                    lo = lo.min(x + x0 * built.sx);
                    hi = hi.max(x + x1 * built.sx);
                }
                if lo > hi { 0.0 } else { (lo + hi) / 2.0 }
            }
        };
        let att_base = match at.gid {
            Some(bg) if single => f.top_accent(bg).map_or(b.w / 2.0 + b.ic / 2.0, |v| v * k),
            _ => b.w / 2.0,
        };
        let dx = att_base - att_acc * k;
        let up = (b.h / k - f.c.accent_base).max(0.0);
        let (bw, bh) = (b.w, b.h);
        let mut out = HBox::default();
        out.add(b, 0.0, 0.0);
        out.items.extend(built_items(&built, k, dx, up));
        out.h = bh.max((built.y1 + up) * k);
        out.w = bw;
        Ok(At { cls: at.cls, b: out, ch: false, gid: None })
    }

    fn brace(&mut self, body: &[Node], over: bool, st: St) -> R<At> {
        let k = self.k(st);
        let b = self.list(body, st)?;
        let g = self.glyph(if over { '⏞' } else { '⏟' })?;
        let built = self.hbuild(g, b.w / k);
        let gap = 0.1 * self.f.upem;
        let up = if over { b.h / k + gap - built.y0 } else { -(b.d / k + gap) - built.y1 };
        let bww = built.w * k;
        let (bw, bh, bd) = (b.w, b.h, b.d);
        let w = bw.max(bww);
        let mut out = HBox::default();
        out.add(b, (w - bw) / 2.0, 0.0);
        out.items.extend(built_items(&built, k, (w - bww) / 2.0, up));
        out.w = w;
        out.h = bh.max((built.y1 + up) * k);
        out.d = bd.max(-(built.y0 + up) * k);
        Ok(At::ord(out))
    }

    fn xarrow(&mut self, c: char, over: &[Node], under: Option<&[Node]>, st: St) -> R<At> {
        let k = self.k(st);
        let em = self.size(st);
        let ob = self.list(over, st.sup())?;
        let ub = match under {
            Some(l) => Some(self.list(l, st.sub())?),
            None => None,
        };
        let tw = (ob.w.max(ub.as_ref().map_or(0.0, |u| u.w)) + 0.8 * em).max(1.6 * em);
        let g = self.glyph(c)?;
        let built = self.hbuild(g, tw / k);
        let aw = built.w * k;
        let w = aw.max(tw);
        let gap = 0.15 * em;
        let mut out = HBox::default();
        out.items.extend(built_items(&built, k, (w - aw) / 2.0, 0.0));
        out.h = built.y1 * k;
        out.d = (-built.y0 * k).max(0.0);
        let (ow, od) = (ob.w, ob.d);
        out.add(ob, (w - ow) / 2.0, -(built.y1 * k + gap + od));
        if let Some(u) = ub {
            let (uw, uh) = (u.w, u.h);
            out.add(u, (w - uw) / 2.0, -built.y0 * k + gap + uh);
        }
        out.w = w;
        Ok(At { cls: Class::Rel, b: out, ch: false, gid: None })
    }

    fn array(&mut self, rows: &[Vec<Vec<Node>>], cols: &[Align], kind: ArrKind, st: St) -> R<At> {
        let cst = match kind {
            ArrKind::Aligned | ArrKind::Gathered => st,
            ArrKind::Small | ArrKind::Substack => st.sup(),
            _ => {
                if st.s == Style::D {
                    st.with(Style::T)
                } else {
                    st
                }
            }
        };
        let em = self.size(cst);
        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let align = |c: usize| -> Align {
            match kind {
                ArrKind::Cases => Align::L,
                ArrKind::Aligned => {
                    if c.is_multiple_of(2) {
                        Align::R
                    } else {
                        Align::L
                    }
                }
                ArrKind::Array => cols.get(c).copied().unwrap_or(Align::C),
                _ => Align::C,
            }
        };
        let mut cells: Vec<Vec<HBox>> = Vec::with_capacity(rows.len());
        for row in rows {
            let mut out = Vec::with_capacity(row.len());
            for (ci, cell) in row.iter().enumerate() {
                let b = if kind == ArrKind::Aligned && ci % 2 == 1 {
                    // `a &= b`: the right half starts with an empty atom so `=` keeps its spacing
                    let mut v = Vec::with_capacity(cell.len() + 1);
                    v.push(Node::Group(vec![]));
                    v.extend(cell.iter().cloned());
                    self.list(&v, cst)?
                } else {
                    self.list(cell, cst)?
                };
                out.push(b);
            }
            cells.push(out);
        }
        let mut colw = vec![0.0f32; ncols];
        for row in &cells {
            for (c, b) in row.iter().enumerate() {
                colw[c] = colw[c].max(b.w);
            }
        }
        let (strut_h, strut_d, row_gap) = match kind {
            ArrKind::Substack | ArrKind::Small => (0.7 * em, 0.25 * em, 0.0),
            ArrKind::Aligned | ArrKind::Gathered => (0.84 * em, 0.36 * em, 0.25 * em),
            _ => (0.84 * em, 0.36 * em, 0.0),
        };
        let col_gap = |c: usize| -> f32 {
            match kind {
                ArrKind::Aligned => {
                    if c % 2 == 1 {
                        2.0 * em
                    } else {
                        0.0
                    }
                }
                ArrKind::Small => 0.5 * em,
                ArrKind::Gathered | ArrKind::Substack => 0.0,
                _ => em,
            }
        };
        let row_hd: Vec<(f32, f32)> = cells
            .iter()
            .map(|r| {
                let h = r.iter().map(|b| b.h).fold(strut_h, f32::max);
                let d = r.iter().map(|b| b.d).fold(strut_d, f32::max);
                (h, d)
            })
            .collect();
        let total: f32 = row_hd.iter().map(|(h, d)| h + d).sum::<f32>() + row_gap * rows.len().saturating_sub(1) as f32;
        let a = self.f.c.axis * self.k(st);
        let top = a + total / 2.0;
        let mut xs = Vec::with_capacity(ncols);
        let mut x = 0.0f32;
        for (c, w) in colw.iter().enumerate() {
            if c > 0 {
                x += col_gap(c - 1);
            }
            xs.push(x);
            x += w;
        }
        let width = x;
        let mut out = HBox::default();
        let mut y = -top;
        for (ri, row) in cells.into_iter().enumerate() {
            let (h, d) = row_hd[ri];
            let base = y + h;
            for (c, b) in row.into_iter().enumerate() {
                let slack = colw[c] - b.w;
                let dx = match align(c) {
                    Align::L => 0.0,
                    Align::C => slack / 2.0,
                    Align::R => slack,
                };
                out.add(b, xs[c] + dx, base);
            }
            y = base + d + row_gap;
        }
        out.w = width;
        out.h = out.h.max(top);
        out.d = out.d.max(total - top);
        Ok(At::ord(out))
    }
}

/// Glyph items of a built (stretched) glyph at `dx`, raised by `up` font units.
fn built_items(b: &Built, k: f32, dx: f32, up: f32) -> Vec<Item> {
    b.parts
        .iter()
        .map(|&(g, x, y)| Item::Glyph { x: dx + x * k, y: -(y + up) * k, gid: g, sx: k * b.sx, sy: k * b.sy })
        .collect()
}
