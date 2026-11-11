//! Symbol tables: TeX command names to Unicode code points and atom classes, and the mapping of
//! letters to the Unicode mathematical alphanumerics (`\mathbf`, `\mathbb`, ...).

/// TeX atom classes (they decide the spacing between neighbours).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
    Inner,
}

use Class::*;

/// Letter styles (`\mathbf` & co.). `Normal` is math italic for letters, upright for digits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Font {
    Normal,
    Rm,
    It,
    Bf,
    BfIt,
    Bb,
    Cal,
    Frak,
    Sf,
    Tt,
}

/// Commands without arguments that stand for one symbol.
static SYMBOLS: &[(&str, char, Class)] = &[
    // Greek capitals (upright, like TeX)
    ("Gamma", 'Γ', Ord),
    ("Delta", 'Δ', Ord),
    ("Theta", 'Θ', Ord),
    ("Lambda", 'Λ', Ord),
    ("Xi", 'Ξ', Ord),
    ("Pi", 'Π', Ord),
    ("Sigma", 'Σ', Ord),
    ("Upsilon", 'Υ', Ord),
    ("Phi", 'Φ', Ord),
    ("Psi", 'Ψ', Ord),
    ("Omega", 'Ω', Ord),
    // ordinary symbols
    ("infty", '∞', Ord),
    ("partial", '∂', Ord),
    ("nabla", '∇', Ord),
    ("emptyset", '∅', Ord),
    ("varnothing", '∅', Ord),
    ("forall", '∀', Ord),
    ("exists", '∃', Ord),
    ("nexists", '∄', Ord),
    ("neg", '¬', Ord),
    ("lnot", '¬', Ord),
    ("aleph", 'ℵ', Ord),
    ("hbar", 'ℏ', Ord),
    ("hslash", 'ℏ', Ord),
    ("ell", 'ℓ', Ord),
    ("wp", '℘', Ord),
    ("Re", 'ℜ', Ord),
    ("Im", 'ℑ', Ord),
    ("imath", '\u{1D6A4}', Ord),
    ("jmath", '\u{1D6A5}', Ord),
    ("angle", '∠', Ord),
    ("measuredangle", '∡', Ord),
    ("triangle", '△', Ord),
    ("square", '□', Ord),
    ("Box", '□', Ord),
    ("blacksquare", '■', Ord),
    ("lozenge", '◊', Ord),
    ("top", '⊤', Ord),
    ("bot", '⊥', Ord),
    ("prime", '′', Ord),
    ("flat", '♭', Ord),
    ("natural", '♮', Ord),
    ("sharp", '♯', Ord),
    ("clubsuit", '♣', Ord),
    ("diamondsuit", '♢', Ord),
    ("heartsuit", '♡', Ord),
    ("spadesuit", '♠', Ord),
    ("S", '§', Ord),
    ("P", '¶', Ord),
    ("checkmark", '✓', Ord),
    ("degree", '°', Ord),
    ("backslash", '\\', Ord),
    ("vert", '|', Ord),
    ("Vert", '‖', Ord),
    ("|", '‖', Ord),
    ("%", '%', Ord),
    ("$", '$', Ord),
    ("&", '&', Ord),
    ("#", '#', Ord),
    ("_", '_', Ord),
    ("ldots", '…', Inner),
    ("dots", '…', Inner),
    ("dotsc", '…', Inner),
    ("dotso", '…', Inner),
    ("cdots", '⋯', Inner),
    ("dotsb", '⋯', Inner),
    ("dotsm", '⋯', Inner),
    ("dotsi", '⋯', Inner),
    ("vdots", '⋮', Ord),
    ("ddots", '⋱', Inner),
    ("iddots", '⋰', Inner),
    ("therefore", '∴', Rel),
    ("because", '∵', Rel),
    // binary operators
    ("pm", '±', Bin),
    ("mp", '∓', Bin),
    ("times", '×', Bin),
    ("div", '÷', Bin),
    ("cdot", '⋅', Bin),
    ("centerdot", '⋅', Bin),
    ("ast", '∗', Bin),
    ("star", '⋆', Bin),
    ("circ", '∘', Bin),
    ("bullet", '∙', Bin),
    ("oplus", '⊕', Bin),
    ("ominus", '⊖', Bin),
    ("otimes", '⊗', Bin),
    ("oslash", '⊘', Bin),
    ("odot", '⊙', Bin),
    ("bigcirc", '◯', Bin),
    ("wedge", '∧', Bin),
    ("land", '∧', Bin),
    ("vee", '∨', Bin),
    ("lor", '∨', Bin),
    ("setminus", '∖', Bin),
    ("smallsetminus", '∖', Bin),
    ("cup", '∪', Bin),
    ("cap", '∩', Bin),
    ("sqcup", '⊔', Bin),
    ("sqcap", '⊓', Bin),
    ("uplus", '⊎', Bin),
    ("amalg", '⨿', Bin),
    ("wr", '≀', Bin),
    ("diamond", '⋄', Bin),
    ("triangleleft", '◁', Bin),
    ("triangleright", '▷', Bin),
    ("lhd", '⊲', Bin),
    ("rhd", '⊳', Bin),
    ("unlhd", '⊴', Bin),
    ("unrhd", '⊵', Bin),
    ("dagger", '†', Bin),
    ("ddagger", '‡', Bin),
    ("dotplus", '∔', Bin),
    ("ltimes", '⋉', Bin),
    ("rtimes", '⋊', Bin),
    ("boxplus", '⊞', Bin),
    ("boxtimes", '⊠', Bin),
    ("boxminus", '⊟', Bin),
    ("boxdot", '⊡', Bin),
    // relations
    ("leq", '≤', Rel),
    ("le", '≤', Rel),
    ("geq", '≥', Rel),
    ("ge", '≥', Rel),
    ("neq", '≠', Rel),
    ("ne", '≠', Rel),
    ("approx", '≈', Rel),
    ("approxeq", '≊', Rel),
    ("equiv", '≡', Rel),
    ("sim", '∼', Rel),
    ("simeq", '≃', Rel),
    ("cong", '≅', Rel),
    ("ncong", '≇', Rel),
    ("propto", '∝', Rel),
    ("varpropto", '∝', Rel),
    ("ll", '≪', Rel),
    ("gg", '≫', Rel),
    ("lll", '⋘', Rel),
    ("ggg", '⋙', Rel),
    ("subset", '⊂', Rel),
    ("supset", '⊃', Rel),
    ("subseteq", '⊆', Rel),
    ("supseteq", '⊇', Rel),
    ("subsetneq", '⊊', Rel),
    ("supsetneq", '⊋', Rel),
    ("nsubseteq", '⊈', Rel),
    ("nsupseteq", '⊉', Rel),
    ("sqsubset", '⊏', Rel),
    ("sqsupset", '⊐', Rel),
    ("sqsubseteq", '⊑', Rel),
    ("sqsupseteq", '⊒', Rel),
    ("in", '∈', Rel),
    ("ni", '∋', Rel),
    ("owns", '∋', Rel),
    ("notin", '∉', Rel),
    ("perp", '⊥', Rel),
    ("parallel", '∥', Rel),
    ("nparallel", '∦', Rel),
    ("mid", '∣', Rel),
    ("nmid", '∤', Rel),
    ("models", '⊨', Rel),
    ("vDash", '⊨', Rel),
    ("vdash", '⊢', Rel),
    ("dashv", '⊣', Rel),
    ("Vdash", '⊩', Rel),
    ("prec", '≺', Rel),
    ("succ", '≻', Rel),
    ("preceq", '⪯', Rel),
    ("succeq", '⪰', Rel),
    ("asymp", '≍', Rel),
    ("doteq", '≐', Rel),
    ("bowtie", '⋈', Rel),
    ("smile", '⌣', Rel),
    ("frown", '⌢', Rel),
    ("leqslant", '⩽', Rel),
    ("geqslant", '⩾', Rel),
    ("lesssim", '≲', Rel),
    ("gtrsim", '≳', Rel),
    ("lessgtr", '≶', Rel),
    ("gtrless", '≷', Rel),
    ("nless", '≮', Rel),
    ("ngtr", '≯', Rel),
    ("nleq", '≰', Rel),
    ("ngeq", '≱', Rel),
    ("coloneqq", '≔', Rel),
    ("coloneq", '≔', Rel),
    ("eqqcolon", '≕', Rel),
    ("triangleq", '≜', Rel),
    ("vartriangleleft", '⊲', Rel),
    ("vartriangleright", '⊳', Rel),
    // arrows
    ("leftarrow", '←', Rel),
    ("gets", '←', Rel),
    ("rightarrow", '→', Rel),
    ("to", '→', Rel),
    ("leftrightarrow", '↔', Rel),
    ("Leftarrow", '⇐', Rel),
    ("Rightarrow", '⇒', Rel),
    ("Leftrightarrow", '⇔', Rel),
    ("iff", '⟺', Rel),
    ("implies", '⟹', Rel),
    ("impliedby", '⟸', Rel),
    ("longleftarrow", '⟵', Rel),
    ("longrightarrow", '⟶', Rel),
    ("longleftrightarrow", '⟷', Rel),
    ("Longleftarrow", '⟸', Rel),
    ("Longrightarrow", '⟹', Rel),
    ("Longleftrightarrow", '⟺', Rel),
    ("mapsto", '↦', Rel),
    ("longmapsto", '⟼', Rel),
    ("hookleftarrow", '↩', Rel),
    ("hookrightarrow", '↪', Rel),
    ("uparrow", '↑', Rel),
    ("downarrow", '↓', Rel),
    ("updownarrow", '↕', Rel),
    ("Uparrow", '⇑', Rel),
    ("Downarrow", '⇓', Rel),
    ("Updownarrow", '⇕', Rel),
    ("nearrow", '↗', Rel),
    ("searrow", '↘', Rel),
    ("nwarrow", '↖', Rel),
    ("swarrow", '↙', Rel),
    ("leftharpoonup", '↼', Rel),
    ("leftharpoondown", '↽', Rel),
    ("rightharpoonup", '⇀', Rel),
    ("rightharpoondown", '⇁', Rel),
    ("rightleftharpoons", '⇌', Rel),
    ("leftrightharpoons", '⇋', Rel),
    ("leadsto", '⇝', Rel),
    ("rightsquigarrow", '⇝', Rel),
    ("nrightarrow", '↛', Rel),
    ("nleftarrow", '↚', Rel),
    ("nRightarrow", '⇏', Rel),
    ("nLeftarrow", '⇍', Rel),
    ("nleftrightarrow", '↮', Rel),
    ("nLeftrightarrow", '⇎', Rel),
    ("twoheadrightarrow", '↠', Rel),
    ("twoheadleftarrow", '↞', Rel),
    ("rightleftarrows", '⇄', Rel),
    ("leftrightarrows", '⇆', Rel),
    ("circlearrowleft", '↺', Rel),
    ("circlearrowright", '↻', Rel),
    // delimiters used as plain symbols
    ("langle", '⟨', Open),
    ("rangle", '⟩', Close),
    ("{", '{', Open),
    ("}", '}', Close),
    ("lbrace", '{', Open),
    ("rbrace", '}', Close),
    ("lbrack", '[', Open),
    ("rbrack", ']', Close),
    ("lfloor", '⌊', Open),
    ("rfloor", '⌋', Close),
    ("lceil", '⌈', Open),
    ("rceil", '⌉', Close),
    ("lvert", '|', Open),
    ("rvert", '|', Close),
    ("lVert", '‖', Open),
    ("rVert", '‖', Close),
    ("lgroup", '⟮', Open),
    ("rgroup", '⟯', Close),
    ("ulcorner", '⌜', Open),
    ("urcorner", '⌝', Close),
    ("llcorner", '⌞', Open),
    ("lrcorner", '⌟', Close),
    // punctuation
    ("colon", ':', Punct),
    ("cdotp", '⋅', Punct),
    ("ldotp", '.', Punct),
];

/// `\name` -> (char, class) for plain symbols.
pub fn symbol(name: &str) -> Option<(char, Class)> {
    SYMBOLS.iter().find(|(n, ..)| *n == name).map(|&(_, c, k)| (c, k))
}

/// Lowercase Greek (`\alpha` ...): the upright code point; [`styled`] makes it italic.
static GREEK: &[(&str, char)] = &[
    ("alpha", 'α'),
    ("beta", 'β'),
    ("gamma", 'γ'),
    ("delta", 'δ'),
    ("epsilon", 'ϵ'),
    ("varepsilon", 'ε'),
    ("zeta", 'ζ'),
    ("eta", 'η'),
    ("theta", 'θ'),
    ("vartheta", 'ϑ'),
    ("iota", 'ι'),
    ("kappa", 'κ'),
    ("varkappa", 'ϰ'),
    ("lambda", 'λ'),
    ("mu", 'μ'),
    ("nu", 'ν'),
    ("xi", 'ξ'),
    ("omicron", 'ο'),
    ("pi", 'π'),
    ("varpi", 'ϖ'),
    ("rho", 'ρ'),
    ("varrho", 'ϱ'),
    ("sigma", 'σ'),
    ("varsigma", 'ς'),
    ("tau", 'τ'),
    ("upsilon", 'υ'),
    ("phi", 'ϕ'),
    ("varphi", 'φ'),
    ("chi", 'χ'),
    ("psi", 'ψ'),
    ("omega", 'ω'),
    ("varGamma", 'Γ'),
    ("varDelta", 'Δ'),
    ("varTheta", 'Θ'),
    ("varLambda", 'Λ'),
    ("varXi", 'Ξ'),
    ("varPi", 'Π'),
    ("varSigma", 'Σ'),
    ("varUpsilon", 'Υ'),
    ("varPhi", 'Φ'),
    ("varPsi", 'Ψ'),
    ("varOmega", 'Ω'),
];

/// Lowercase Greek letter (and the `\var` capitals, which TeX sets in italic).
pub fn greek(name: &str) -> Option<char> {
    GREEK.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
}

/// Large operators: (char, is an integral). Integrals take their limits as scripts.
static BIG_OPS: &[(&str, char, bool)] = &[
    ("sum", '∑', false),
    ("prod", '∏', false),
    ("coprod", '∐', false),
    ("int", '∫', true),
    ("intop", '∫', true),
    ("iint", '∬', true),
    ("iiint", '∭', true),
    ("iiiint", '⨌', true),
    ("oint", '∮', true),
    ("oiint", '∯', true),
    ("oiiint", '∰', true),
    ("bigcup", '⋃', false),
    ("bigcap", '⋂', false),
    ("bigoplus", '⨁', false),
    ("bigotimes", '⨂', false),
    ("bigodot", '⨀', false),
    ("bigvee", '⋁', false),
    ("bigwedge", '⋀', false),
    ("bigsqcup", '⨆', false),
    ("biguplus", '⨄', false),
];

pub fn big_op(name: &str) -> Option<(char, bool)> {
    BIG_OPS.iter().find(|(n, ..)| *n == name).map(|&(_, c, i)| (c, i))
}

/// True for the code points of [`BIG_OPS`] (typed directly in the source).
pub fn big_op_char(c: char) -> Option<bool> {
    BIG_OPS.iter().find(|(_, ch, _)| *ch == c).map(|&(_, _, i)| i)
}

/// Operator names set upright: (text, takes limits in display style).
static FUNCS: &[(&str, &str, bool)] = &[
    ("arccos", "arccos", false),
    ("arcsin", "arcsin", false),
    ("arctan", "arctan", false),
    ("arg", "arg", false),
    ("cos", "cos", false),
    ("cosh", "cosh", false),
    ("cot", "cot", false),
    ("coth", "coth", false),
    ("csc", "csc", false),
    ("deg", "deg", false),
    ("det", "det", true),
    ("dim", "dim", false),
    ("exp", "exp", false),
    ("gcd", "gcd", true),
    ("hom", "hom", false),
    ("inf", "inf", true),
    ("ker", "ker", false),
    ("lg", "lg", false),
    ("lim", "lim", true),
    ("liminf", "lim inf", true),
    ("limsup", "lim sup", true),
    ("ln", "ln", false),
    ("log", "log", false),
    ("max", "max", true),
    ("min", "min", true),
    ("Pr", "Pr", true),
    ("sec", "sec", false),
    ("sin", "sin", false),
    ("sinh", "sinh", false),
    ("sup", "sup", true),
    ("tan", "tan", false),
    ("tanh", "tanh", false),
    ("argmax", "arg max", true),
    ("argmin", "arg min", true),
];

pub fn function(name: &str) -> Option<(&'static str, bool)> {
    FUNCS.iter().find(|(n, ..)| *n == name).map(|&(_, t, l)| (t, l))
}

/// Accents: (combining char, stretches with its base).
static ACCENTS: &[(&str, char, bool)] = &[
    ("hat", '\u{302}', false),
    ("widehat", '\u{302}', true),
    ("check", '\u{30C}', false),
    ("widecheck", '\u{30C}', true),
    ("tilde", '\u{303}', false),
    ("widetilde", '\u{303}', true),
    ("acute", '\u{301}', false),
    ("grave", '\u{300}', false),
    ("dot", '\u{307}', false),
    ("ddot", '\u{308}', false),
    ("dddot", '\u{20DB}', false),
    ("breve", '\u{306}', false),
    ("bar", '\u{304}', false),
    ("vec", '\u{20D7}', false),
    ("mathring", '\u{30A}', false),
    ("overrightarrow", '\u{20D7}', true),
    ("overleftarrow", '\u{20D6}', true),
    ("overleftrightarrow", '\u{20E1}', true),
];

pub fn accent(name: &str) -> Option<(char, bool)> {
    ACCENTS.iter().find(|(n, ..)| *n == name).map(|&(_, c, w)| (c, w))
}

/// Font commands that take an argument (`\mathbf{x}`).
pub fn font_cmd(name: &str) -> Option<Font> {
    Some(match name {
        "mathrm" | "mathup" => Font::Rm,
        "mathnormal" => Font::Normal,
        "mathit" => Font::It,
        "mathbf" => Font::Bf,
        "boldsymbol" | "bm" | "pmb" => Font::BfIt,
        "mathbb" | "Bbb" => Font::Bb,
        "mathcal" | "mathscr" => Font::Cal,
        "mathfrak" | "frak" => Font::Frak,
        "mathsf" => Font::Sf,
        "mathtt" => Font::Tt,
        _ => return None,
    })
}

/// Old-style font switches (`{\rm d}`): they change the font until the end of the group.
pub fn font_switch(name: &str) -> Option<Font> {
    Some(match name {
        "rm" => Font::Rm,
        "it" => Font::It,
        "bf" => Font::Bf,
        "cal" => Font::Cal,
        "sf" => Font::Sf,
        "tt" => Font::Tt,
        _ => return None,
    })
}

/// `\not` followed by `next` (a char or a command name): the negated relation, when Unicode has one.
pub fn negated(next: &str) -> Option<char> {
    Some(match next {
        "=" => '≠',
        "<" => '≮',
        ">" => '≯',
        "in" => '∉',
        "ni" => '∌',
        "equiv" => '≢',
        "sim" => '≁',
        "approx" => '≉',
        "simeq" => '≄',
        "cong" => '≇',
        "subset" => '⊄',
        "supset" => '⊅',
        "subseteq" => '⊈',
        "supseteq" => '⊉',
        "le" | "leq" => '≰',
        "ge" | "geq" => '≱',
        "mid" => '∤',
        "parallel" => '∦',
        "exists" => '∄',
        _ => return None,
    })
}

/// Code points of the letters `a-z`, `A-Z`, `0-9` in a style (Unicode mathematical alphanumerics,
/// with the holes that live in the Letterlike Symbols block), and Greek in italic / bold.
pub fn styled(c: char, font: Font) -> char {
    if let Some(g) = greek_styled(c, font) {
        return g;
    }
    if let Some(h) = hole(c, font) {
        return h;
    }
    let (upper, lower, digit): (u32, u32, Option<u32>) = match font {
        Font::Normal | Font::It => (0x1D434, 0x1D44E, None),
        Font::Rm => return c,
        Font::Bf => (0x1D400, 0x1D41A, Some(0x1D7CE)),
        Font::BfIt => (0x1D468, 0x1D482, Some(0x1D7CE)),
        Font::Bb => (0x1D538, 0x1D552, Some(0x1D7D8)),
        Font::Cal => (0x1D49C, 0x1D4B6, None),
        Font::Frak => (0x1D504, 0x1D51E, None),
        Font::Sf => (0x1D5A0, 0x1D5BA, Some(0x1D7E2)),
        Font::Tt => (0x1D670, 0x1D68A, Some(0x1D7F6)),
    };
    let u = c as u32;
    let v = match c {
        'A'..='Z' => upper + (u - 'A' as u32),
        'a'..='z' => lower + (u - 'a' as u32),
        '0'..='9' => match digit {
            Some(d) => d + (u - '0' as u32),
            None => return c,
        },
        _ => return c,
    };
    char::from_u32(v).unwrap_or(c)
}

/// Letters whose styled form is not in the Mathematical Alphanumeric block.
fn hole(c: char, font: Font) -> Option<char> {
    Some(match (font, c) {
        (Font::Normal | Font::It, 'h') => '\u{210E}',
        (Font::Cal, 'B') => '\u{212C}',
        (Font::Cal, 'E') => '\u{2130}',
        (Font::Cal, 'F') => '\u{2131}',
        (Font::Cal, 'H') => '\u{210B}',
        (Font::Cal, 'I') => '\u{2110}',
        (Font::Cal, 'L') => '\u{2112}',
        (Font::Cal, 'M') => '\u{2133}',
        (Font::Cal, 'R') => '\u{211B}',
        (Font::Cal, 'e') => '\u{212F}',
        (Font::Cal, 'g') => '\u{210A}',
        (Font::Cal, 'o') => '\u{2134}',
        (Font::Frak, 'C') => '\u{212D}',
        (Font::Frak, 'H') => '\u{210C}',
        (Font::Frak, 'I') => '\u{2111}',
        (Font::Frak, 'R') => '\u{211C}',
        (Font::Frak, 'Z') => '\u{2128}',
        (Font::Bb, 'C') => '\u{2102}',
        (Font::Bb, 'H') => '\u{210D}',
        (Font::Bb, 'N') => '\u{2115}',
        (Font::Bb, 'P') => '\u{2119}',
        (Font::Bb, 'Q') => '\u{211A}',
        (Font::Bb, 'R') => '\u{211D}',
        (Font::Bb, 'Z') => '\u{2124}',
        _ => return None,
    })
}

/// Greek in a style: lowercase is italic in the normal style (TeX), capitals stay upright unless
/// asked for italic / bold.
fn greek_styled(c: char, font: Font) -> Option<char> {
    let u = c as u32;
    let lower = (0x3B1..=0x3C9).contains(&u);
    let upper = (0x391..=0x3A9).contains(&u);
    // italic forms of the variant letters
    let var_it = match c {
        'ϵ' => Some(0x1D716),
        'ϑ' => Some(0x1D717),
        'ϰ' => Some(0x1D718),
        'ϕ' => Some(0x1D719),
        'ϱ' => Some(0x1D71A),
        'ϖ' => Some(0x1D71B),
        _ => None,
    };
    let v = match (font, var_it) {
        (Font::Normal | Font::It, _) if lower => 0x1D6FC + (u - 0x3B1),
        (Font::Normal | Font::It, Some(v)) => v,
        _ => match font {
            Font::It if upper => 0x1D6E2 + (u - 0x391),
            Font::Bf if lower => 0x1D6C2 + (u - 0x3B1),
            Font::Bf if upper => 0x1D6A8 + (u - 0x391),
            Font::BfIt if lower => 0x1D736 + (u - 0x3B1),
            Font::BfIt if upper => 0x1D71C + (u - 0x391),
            _ => return None,
        },
    };
    char::from_u32(v)
}

/// Class of a character typed directly in the source.
pub fn class_of(c: char) -> Class {
    if let Some(&(_, _, k)) = SYMBOLS.iter().find(|(_, ch, _)| *ch == c) {
        return k;
    }
    match c {
        '+' | '−' | '∗' => Bin,
        '=' | '<' | '>' | ':' => Rel,
        ',' | ';' => Punct,
        '(' | '[' => Open,
        ')' | ']' | '!' | '?' => Close,
        _ => Ord,
    }
}
