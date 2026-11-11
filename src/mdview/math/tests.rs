use super::parse::{ArrKind, Node, parse};
use super::syms::{Class, Font, styled};
use super::*;

/// Body-font stand-in: 0.5 em per char.
struct Fake;

impl TextMeasure for Fake {
    fn text(&mut self, s: &str, size: f32, _bold: bool) -> TextMetrics {
        TextMetrics { w: s.chars().count() as f32 * size * 0.5, asc: size * 0.75, desc: size * 0.22, bl: size }
    }
}

fn lay(src: &str, display: bool) -> MathLay {
    layout(src, 20.0, display, &mut Fake).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

fn gid(c: char) -> u16 {
    font::with_font(|f| f.glyph(c)).flatten().unwrap_or_else(|| panic!("no glyph for {c}"))
}

/// Glyph items of `l` drawing `c` (by its glyph id): `(x, y, sx, sy)`.
fn glyphs_of(l: &MathLay, c: char) -> Vec<(f32, f32, f32, f32)> {
    let g = gid(c);
    l.items
        .iter()
        .filter_map(|it| match *it {
            Item::Glyph { x, y, gid, sx, sy } if gid == g => Some((x, y, sx, sy)),
            _ => None,
        })
        .collect()
}

/// `c`'s glyph with its vertical size variants and assembly pieces.
fn family(c: char) -> Vec<u16> {
    let g = gid(c);
    font::with_font(|f| {
        let mut v = vec![g];
        v.extend(f.variants(g, true).iter().map(|v| v.0));
        v.extend(f.assembly(g, true).unwrap_or_default().iter().map(|p| p.g));
        v
    })
    .unwrap()
}

fn rules(l: &MathLay) -> Vec<(f32, f32, f32, f32)> {
    l.items
        .iter()
        .filter_map(|it| match *it {
            Item::Rule { x, y, w, h } => Some((x, y, w, h)),
            _ => None,
        })
        .collect()
}

/// Ink extent (top, bottom) in px, y down, of every glyph item of `l`.
fn ink_y(l: &MathLay, pick: impl Fn(u16) -> bool) -> (f32, f32) {
    font::with_font(|f| {
        let (mut t, mut b) = (f32::MAX, f32::MIN);
        for it in &l.items {
            if let Item::Glyph { y, gid, sy, .. } = *it
                && pick(gid)
            {
                let (_, y0, _, y1) = f.bbox(gid);
                t = t.min(y - y1 * sy);
                b = b.max(y - y0 * sy);
            }
        }
        (t, b)
    })
    .unwrap()
}

fn err_of(src: &str) -> MathErr {
    match layout(src, 20.0, false, &mut Fake) {
        Ok(_) => panic!("{src} should not typeset"),
        Err(e) => e,
    }
}

// ---- parser ----

#[test]
fn parses_letters_operators_and_classes() {
    let n = parse("a+b=c").unwrap();
    let classes: Vec<Class> =
        n.iter().map(|x| if let Node::Char { class, .. } = x { *class } else { Class::Inner }).collect();
    assert_eq!(classes, [Class::Ord, Class::Bin, Class::Ord, Class::Rel, Class::Ord]);
    // letters are math italic, digits upright, h has its own code point
    assert!(matches!(n[0], Node::Char { c: '\u{1D44E}', .. }));
    assert_eq!(styled('h', Font::Normal), '\u{210E}');
    assert_eq!(styled('2', Font::Normal), '2');
    assert_eq!(styled('R', Font::Bb), 'ℝ');
    assert_eq!(styled('A', Font::Bb), '\u{1D538}');
    assert_eq!(styled('L', Font::Cal), 'ℒ');
    assert_eq!(styled('x', Font::Bf), '\u{1D431}');
    assert_eq!(styled('x', Font::Rm), 'x');
    assert_eq!(styled('α', Font::Normal), '\u{1D6FC}');
    assert_eq!(styled('Γ', Font::Normal), 'Γ', "Greek capitals stay upright");
}

#[test]
fn parses_every_construct() {
    let cases = [
        r"x_i^2",
        r"x^{y^{z}}",
        r"\frac{a}{b} \dfrac12 \tfrac{1}{2} \cfrac{a}{b}",
        r"\sqrt{x} \sqrt[3]{x+1}",
        r"\left( \frac{a}{b} \right] \left\{ x \middle| y \right\} \left. f \right|_{0}",
        r"\langle a \rangle \lfloor x \rfloor \lceil y \rceil \| v \|",
        r"\binom{n}{k} \dbinom nk \tbinom{n}{k}",
        r"\hat x \bar y \vec v \tilde a \dot b \ddot c \check d \widehat{xyz} \overline{AB} \underline{CD}",
        r"\text{if } x \mathrm{d}x \mathbf{v} \mathit{x} \mathbb{R} \mathcal{L} \mathfrak{g} \mathsf{A} \mathtt{t} \boldsymbol\alpha",
        r"a\,b\:c\;d\quad e\qquad f\!g \hspace{1em} \kern3mu",
        r"\begin{matrix} a & b \\ c & d \end{matrix}",
        r"\begin{pmatrix} a & b \\ c & d \end{pmatrix} \begin{bmatrix}1\end{bmatrix} \begin{vmatrix}1\end{vmatrix}",
        r"\begin{Bmatrix}1\end{Bmatrix} \begin{Vmatrix}1\end{Vmatrix} \begin{smallmatrix}a&b\end{smallmatrix}",
        r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
        r"\begin{aligned} a &= b \\ &= c \end{aligned}",
        r"\begin{equation} E = mc^2 \end{equation}",
        r"\begin{array}{c|lr} a & b & c \end{array}",
        r"\sum_{i=1}^n \prod_k \int_0^1 \iint \oint \lim_{x\to 0} \max_i \min \sup \inf",
        r"\sin x \cos y \tan z \log n \ln x \exp t \det A \operatorname{rank} \operatorname*{arg\,max}",
        r"\le \ge \ne \approx \equiv \sim \propto \pm \mp \times \div \cdot \circ \to \rightarrow",
        r"\leftarrow \Rightarrow \Leftrightarrow \mapsto \in \notin \subset \subseteq \cup \cap",
        r"\forall \exists \partial \nabla \infty \emptyset \ldots \cdots \vdots \ddots",
        r"\alpha \beta \gamma \Gamma \Delta \Omega \varepsilon \phi \varphi",
        r"\big( \Big[ \bigg\{ \Bigg| \bigl( \bigr)",
        r"\overbrace{a+b}^{n} \underbrace{c}_{m} \overset{!}{=} \underset{x}{\arg} \stackrel{def}{=}",
        r"\xrightarrow{f} \xleftarrow[a]{b} \not\in \not= \not\perp \phantom{x} \boxed{y}",
        r"\displaystyle \sum \textstyle x \scriptstyle y \color{red} z \textcolor{blue}{w}",
        r"x' f'' \{ a \} \# \% \& \$ \_ ~ a \pmod{p} a \bmod b \mod n",
        r"\sum_{\substack{0<i<m \\ 0<j<n}} P(i,j) {a \over b} {n \choose k}",
        r"a \\ b \\ c",
        r"a &= b \\ c &= d",
    ];
    for src in cases {
        if let Err(e) = parse(src) {
            panic!("{src}: {e:?}");
        }
        let l = lay(src, true);
        assert!(l.w > 0.0 && l.h > 0.0, "{src} has a box: {} x {}", l.w, l.h);
        let l = lay(src, false);
        assert!(l.w > 0.0, "{src} inline");
    }
}

#[test]
fn top_level_rows_become_an_array() {
    let n = parse(r"a \\ b").unwrap();
    assert!(matches!(&n[..], [Node::Array { kind: ArrKind::Gathered, rows, .. }] if rows.len() == 2));
    let n = parse(r"a &= b \\ c &= d \\").unwrap();
    assert!(matches!(&n[..], [Node::Array { kind: ArrKind::Aligned, rows, .. }] if rows.len() == 2));
}

#[test]
fn reports_errors_with_their_position() {
    let e = err_of(r"\frac{a");
    assert_eq!(e.at.0, 5, "the unclosed brace: {e:?}");
    let e = err_of(r"x + \foo + y");
    assert_eq!(e.at, (4, 8), "the unknown command: {e:?}");
    assert!(e.msg.contains("foo"));
    let e = err_of("x^a^b");
    assert_eq!(e.at, (3, 4), "{e:?}");
    let e = err_of(r"\left( x");
    assert_eq!(e.at.0, 0, "{e:?}");
    let e = err_of(r"\begin{foo} a \end{foo}");
    assert!(e.msg.contains("foo"), "{e:?}");
    let e = err_of(r"\begin{matrix} a \end{pmatrix}");
    assert!(e.msg.contains("pmatrix"), "{e:?}");
    let e = err_of("a }");
    assert_eq!(e.at, (2, 3));
    let e = err_of(r"\sqrt[3");
    assert_eq!(e.at.0, 5, "{e:?}");
    err_of(r"x^");
    err_of(r"\frac{1}");
    err_of(r"\right)");
    err_of(r"\end{matrix}");
    err_of(r"a & b \\ {c & d}");
    err_of(r"\left\foo x \right)");
    err_of("\\");
}

#[test]
fn never_panics_on_odd_input() {
    let odd = [
        "",
        " ",
        "{",
        "}",
        "{{{{",
        "}}}}",
        "^",
        "_",
        "^^",
        "x__",
        "\\left",
        "\\right",
        "\\left(",
        "\\begin",
        "\\begin{",
        "\\begin{matrix",
        "\\end",
        "\\frac",
        "\\frac{",
        "\\sqrt[",
        "\\sqrt[]{}",
        "&",
        "\\\\",
        "''''",
        "\\text{",
        "\\text",
        "\\not",
        "\\hspace{",
        "\\hspace{x}",
        "\\kern",
        "\\color",
        "\\big",
        "α^β_γ",
        "😀",
        "\\mathbb{😀}",
        "x^{\\left(\\right.}",
        "{a \\over b \\over c}",
        "\\overbrace",
        "\\substack{",
        "\\operatorname{",
        "%comment",
        "a % b\nc",
        "\\left(\\middle|\\right)",
        "\\boxed{}",
        "\\binom{}{}",
    ];
    for src in odd {
        let _ = layout(src, 16.0, true, &mut Fake);
        let _ = layout(src, 16.0, false, &mut Fake);
    }
    let deep = "{".repeat(500) + &"}".repeat(500);
    assert!(layout(&deep, 16.0, false, &mut Fake).is_err(), "deep nesting is refused, not recursed");
    let deep = "x^".repeat(200) + "x";
    let _ = layout(&deep, 16.0, false, &mut Fake);
}

// ---- layout ----

#[test]
fn fraction_stacks_numerator_over_denominator() {
    let l = lay(r"\frac{a}{bbbb}", true);
    let (num, den) = (glyphs_of(&l, '\u{1D44E}'), glyphs_of(&l, '\u{1D44F}'));
    assert_eq!((num.len(), den.len()), (1, 4));
    assert!(num[0].1 < 0.0 && den[0].1 > 0.0, "num above the baseline, den below: {num:?} {den:?}");
    let r = rules(&l);
    assert_eq!(r.len(), 1, "one fraction bar");
    let (rx, ry, rw, _) = r[0];
    assert!(num[0].1 < ry && den[0].1 > ry, "the bar sits between them");
    // the bar is as wide as the wider part, and the narrow numerator is centered over it
    let den_w = den[3].0 - den[0].0;
    assert!(rw > den_w, "bar {rw} vs den span {den_w}");
    let mid = rx + rw / 2.0;
    assert!(num[0].0 > rx && num[0].0 < mid, "numerator centered: {num:?} bar {r:?}");
    assert!(l.w >= rw);
    // the bar is on the math axis (above the baseline)
    assert!(ry < 0.0);
    // text style is flatter than display style
    let t = lay(r"\frac{a}{bbbb}", false);
    assert!(t.h + t.d < l.h + l.d);
}

#[test]
fn superscripts_are_raised_and_smaller() {
    let l = lay("x^2", false);
    let x = glyphs_of(&l, '\u{1D465}');
    let two = glyphs_of(&l, '2');
    assert_eq!((x.len(), two.len()), (1, 1));
    assert!(two[0].1 < 0.0, "raised");
    assert!(two[0].2 < x[0].2 * 0.8, "smaller: {} vs {}", two[0].2, x[0].2);
    assert!(two[0].0 > x[0].0, "to the right");
    let l = lay("x_2", false);
    assert!(glyphs_of(&l, '2')[0].1 > 0.0, "subscripts are lowered");
    // both scripts: sup above sub, not overlapping
    let l = lay("x_i^2", false);
    let two = glyphs_of(&l, '2');
    let i = glyphs_of(&l, '\u{1D456}');
    assert!(two[0].1 < i[0].1);
    // nested scripts get smaller again
    let l = lay("x^{y^z}", false);
    let (y, z) = (glyphs_of(&l, '\u{1D466}'), glyphs_of(&l, '\u{1D467}'));
    assert!(z[0].2 < y[0].2 && z[0].1 < y[0].1);
}

#[test]
fn big_operator_limits_go_above_and_below_in_display_only() {
    let sum = gid('∑');
    let sums = family('∑');
    let d = lay(r"\sum_{i=1}^{n}", true);
    let (s_top, s_bot) = ink_y(&d, |g| !sums.contains(&g));
    let (op_top, op_bot) = ink_y(&d, |g| sums.contains(&g));
    let n = glyphs_of(&d, '\u{1D45B}');
    let i = glyphs_of(&d, '\u{1D456}');
    assert!(n[0].1 < op_top, "upper limit above the operator: {n:?} op top {op_top}");
    assert!(i[0].1 > op_bot, "lower limit below: {i:?} op bottom {op_bot}");
    assert!(s_top < op_top && s_bot > op_bot);
    assert!(glyphs_of(&d, '∑').is_empty(), "display style uses the larger size variant");
    let t = lay(r"\sum_{i=1}^{n}", false);
    let (t_top, t_bot) = ink_y(&t, |g| sums.contains(&g));
    assert!(t_bot - t_top < op_bot - op_top, "the display sum is taller");
    // inline: the limits are scripts to the right
    let op_right = t
        .items
        .iter()
        .find_map(|it| match *it {
            Item::Glyph { x, gid, sx, .. } if gid == sum => Some(x + font::with_font(|f| f.adv(gid)).unwrap() * sx),
            _ => None,
        })
        .unwrap();
    let n = glyphs_of(&t, '\u{1D45B}');
    assert!(n[0].0 >= op_right - 0.5, "inline limits sit right of the operator: {n:?} vs {op_right}");
    // integrals keep scripts even in display style
    let l = lay(r"\int_0^1", true);
    let one = glyphs_of(&l, '1');
    let int_x = l.items.iter().find_map(|it| if let Item::Glyph { x, .. } = *it { Some(x) } else { None }).unwrap();
    assert!(one[0].0 > int_x + 2.0);
    // \limits forces them under even inline
    let l = lay(r"\sum\limits_{k}", false);
    let (_, ob) = ink_y(&l, |g| g == sum);
    assert!(glyphs_of(&l, '\u{1D458}')[0].1 > ob);
}

#[test]
fn matrix_columns_are_aligned() {
    let l = lay(r"\begin{matrix} a & bbb \\ cc & d \end{matrix}", true);
    let a = glyphs_of(&l, '\u{1D44E}');
    let b = glyphs_of(&l, '\u{1D44F}');
    let c = glyphs_of(&l, '\u{1D450}');
    let d = glyphs_of(&l, '\u{1D451}');
    let adv = |ch: char| font::with_font(|f| f.adv(gid(ch))).unwrap() * a[0].2;
    // centered columns: the centers of a and cc match, and of bbb and d
    let ca = a[0].0 + adv('\u{1D44E}') / 2.0;
    let cc = (c[0].0 + c[1].0 + adv('\u{1D450}')) / 2.0;
    assert!((ca - cc).abs() < 0.6, "column 1 centered: {ca} vs {cc}");
    let cb = (b[0].0 + b[2].0 + adv('\u{1D44F}')) / 2.0;
    let cd = d[0].0 + adv('\u{1D451}') / 2.0;
    assert!((cb - cd).abs() < 0.6, "column 2 centered: {cb} vs {cd}");
    // rows: same baseline within a row, second row lower
    assert!((a[0].1 - b[0].1).abs() < 0.01 && c[0].1 > a[0].1);
    // aligned: right column flush left, left column flush right at the `=`
    let l = lay(r"\begin{aligned} a &= b \\ ccc &= d \end{aligned}", true);
    let eq = glyphs_of(&l, '=');
    assert_eq!(eq.len(), 2);
    assert!((eq[0].0 - eq[1].0).abs() < 0.01, "the = signs line up: {eq:?}");
}

#[test]
fn stretched_delimiters_cover_their_content() {
    let (lp, rp) = (family('('), family(')'));
    let delim = |g: u16| lp.contains(&g) || rp.contains(&g);
    for src in [
        r"\left( \frac{\frac{a}{b}}{\frac{c}{d}} \right)",
        r"\begin{pmatrix} 1 \\ 2 \\ 3 \\ 4 \\ 5 \\ 6 \end{pmatrix}",
        r"\left( \sum_{i=1}^{n} x_i \right)",
    ] {
        let l = lay(src, true);
        let (dt, db) = ink_y(&l, |g| lp.contains(&g));
        let (ct, cb) = ink_y(&l, |g| !delim(g));
        let (dh, ch) = (db - dt, cb - ct);
        assert!(dh >= ch * 0.85 && dh < ch * 2.0, "{src}: delimiter {dt}..{db} vs content {ct}..{cb}");
        // centered on the content (both are centered on the math axis)
        assert!(((dt + db) - (ct + cb)).abs() / 2.0 < ch * 0.2, "{src}: centered");
    }
    // small content keeps the normal glyph
    let l = lay(r"\left( x \right)", false);
    assert_eq!(glyphs_of(&l, '(').len(), 1);
}

#[test]
fn radicals_put_a_rule_over_their_body() {
    let l = lay(r"\sqrt{x^2+y^2}", false);
    let r = rules(&l);
    assert_eq!(r.len(), 1);
    let (rx, ry, rw, _) = r[0];
    let x = glyphs_of(&l, '\u{1D465}');
    assert!(x[0].0 >= rx - 0.01 && x[0].0 < rx + rw, "body under the rule");
    let rad = family('√');
    let (top, _) = ink_y(&l, |g| !rad.contains(&g));
    assert!(ry < top, "the rule is above the body ink");
    let l = lay(r"\sqrt[3]{x}", false);
    let three = glyphs_of(&l, '3');
    assert!(three[0].1 < 0.0 && three[0].2 < x[0].2, "the degree is small and raised");
}

#[test]
fn accents_sit_above_their_base() {
    let l = lay(r"\hat{x}", false);
    let (top_x, _) = ink_y(&l, |g| g == gid('\u{1D465}'));
    let (top_hat, bot_hat) = ink_y(&l, |g| g != gid('\u{1D465}'));
    assert!(bot_hat <= top_x + 1.0 && top_hat < top_x);
    let l = lay(r"\overline{AB}", false);
    assert_eq!(rules(&l).len(), 1);
}

#[test]
fn text_uses_the_body_font_measure() {
    let l = lay(r"\text{if } x", false);
    let t: Vec<&Item> = l.items.iter().filter(|i| matches!(i, Item::Text { .. })).collect();
    assert_eq!(t.len(), 1);
    assert!(matches!(t[0], Item::Text { s, .. } if s == "if "));
}

#[test]
fn cache_keeps_one_layout_per_formula_and_size() {
    let mut c = MathCache::default();
    let a = c.get("x^2", 16.0, false, &mut Fake).unwrap();
    let b = c.get("x^2", 16.0, false, &mut Fake).unwrap();
    assert!(Rc::ptr_eq(&a, &b));
    let d = c.get("x^2", 16.0, true, &mut Fake).unwrap();
    assert!(!Rc::ptr_eq(&a, &d));
    assert_eq!(c.map.len(), 2);
    assert!(c.get(r"\frac{a", 16.0, false, &mut Fake).is_err());
    assert!(c.err(r"\frac{a").is_some());
}

#[test]
fn rasterizes_into_a_mask() {
    let l = lay(r"\frac{1}{2}", true);
    let r = font::with_font(|f| rasterize(f, &l.items, 1.0)).flatten().expect("a mask");
    assert!(r.a.iter().any(|&a| a == 255), "solid coverage somewhere");
    assert!(r.w as f32 >= l.w * 0.8 && (r.h as f32) >= (l.h + l.d) * 0.8);
    // drawn on a canvas, in the given color
    let (w, h) = (80, 80);
    let mut px = vec![0u32; (w * h) as usize];
    let mut cv = Canvas { px: &mut px, w, h };
    blit(&mut cv, &r, 20, 50, Rgba(255, 0, 0, 255), Rect::new(0, 0, w, h));
    assert!(px.iter().any(|&p| p == 0xff0000));
}

#[test]
fn err_pieces_split_the_source() {
    let base = Rgba(1, 1, 1, 255);
    let err = Rgba(2, 2, 2, 255);
    let p = err_pieces(r"x + \foo + y", (4, 8), base, err);
    assert_eq!(p, vec![("x + ".to_string(), base), (r"\foo".to_string(), err), (" + y".to_string(), base)]);
    // past the end: the last char
    let p = err_pieces(r"\frac{a", (5, 99), base, err);
    assert_eq!(p[1], ("{a".to_string(), err));
    let p = err_pieces("ab", (7, 9), base, err);
    assert_eq!(p, vec![("a".to_string(), base), ("b".to_string(), err)]);
}
