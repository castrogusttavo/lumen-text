//! Optional Vim modal editing: the engine. Off by default; `App::vim` is `None` until the
//! `vim_mode` setting (or the "Vim" keymap preset) turns it on, so nothing here runs otherwise.
//!
//! The engine is pure: it works on a [`Buffer`] and a [`Sel`] through [`Ctx`] and answers each key
//! with a [`Feed`] (`src/app_vim.rs` wires it into the key handling, status bar and commands).
//! Supported: Normal / Insert / Visual / Visual-line modes, counts, registers (`"a`, `"_`, `"+`),
//! operators (`d c y > < gu gU g~`) with motions, text objects, `.` repeat, marks, `f t ; ,`,
//! `/ ? n N * #`, `q`/`@` macros (recorded by `macros.rs`) and a few ex commands (`:w :q :wq :x
//! :e :N :s :d`).
//!
//! Text objects: [`TextObjects`] is the hook for a smarter provider (tree-sitter functions,
//! classes, arguments); it is asked first and the local word / paragraph / bracket / quote
//! implementations in this file answer when it returns `None`.
//!
//! Motions use character positions: `w b e W B E 0 ^ $ gg G h j k l f F t T % { } n N * # gE ge
//! ' ` | + - _` and `Ctrl+D` / `Ctrl+U`.

use crate::buffer::{Buffer, EditKind, Sel};
use regex::Regex;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// One key press as the engine sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VKey {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Ctrl(char),
}

/// The system clipboard, as far as registers need it.
pub trait Clip {
    fn get(&mut self) -> String;
    fn set(&mut self, text: &str);
}

/// Hook for text objects beyond the local fallbacks (`iw`, `ip`, brackets, quotes). `kind` is the
/// key after `i` / `a` (`f` for a function, `c` for a class, ...). Return the char range
/// `[start, end)`, or `None` to let the local implementation answer.
pub trait TextObjects: Send {
    fn object(&self, buf: &Buffer, pos: usize, kind: char, inner: bool) -> Option<(usize, usize)>;
}

/// What the app must do after a key.
#[derive(Debug, PartialEq)]
pub enum Feed {
    /// The key is not the engine's (typing in Insert mode): handle it normally.
    Pass,
    /// Handled.
    Done,
    Undo(usize),
    Redo(usize),
    Ex(ExCmd),
    Message(String),
    MacroStart(char),
    MacroStop(char),
    MacroPlay(char, usize),
}

#[derive(Debug, PartialEq, Clone)]
pub enum ExCmd {
    Write,
    Quit { force: bool },
    WriteQuit,
    Edit(String),
}

/// Everything the engine touches for one key.
pub struct Ctx<'a> {
    pub buf: &'a mut Buffer,
    pub sel: &'a mut Sel,
    pub clip: &'a mut dyn Clip,
    /// One indentation level ("    " or "\t") for `>` and `<`.
    pub indent: &'a str,
    /// Visible lines (for `Ctrl+D` / `Ctrl+U`).
    pub page: usize,
}

#[derive(Clone, Default)]
struct Reg {
    text: String,
    lines: bool,
}

pub struct Vim {
    pub mode: Mode,
    pub objects: Option<Box<dyn TextObjects>>,
    /// Macros recorded with `q{reg}`.
    pub macro_regs: HashMap<char, Vec<crate::macros::MacroKey>>,
    macro_rec: Option<char>,
    last_macro: Option<char>,
    pending: Vec<VKey>,
    cur: Vec<VKey>,
    cmdline: Option<(char, String)>,
    regs: HashMap<char, Reg>,
    last_mirror: String,
    find: Option<(char, bool, bool)>,
    search: Option<(String, bool)>,
    marks: HashMap<char, usize>,
    want: Option<usize>,
    vanchor: usize,
    vcursor: usize,
    last_change: Vec<VKey>,
    recording_change: bool,
    replaying: bool,
    dirty: bool,
    ins_keys: Vec<VKey>,
    ins_repeat: usize,
    ins_open: bool,
    in_group: bool,
    gen_id: u64,
}

// ───────────── buffer helpers ─────────────

/// Lines vim sees: the rope has an extra empty one after a final line break.
fn nlines(b: &Buffer) -> usize {
    let n = b.line_count();
    if n > 1 && b.line_len(n - 1) == 0 { n - 1 } else { n }
}

fn lstart(b: &Buffer, l: usize) -> usize {
    b.line_start(l.min(b.line_count()))
}

fn llen(b: &Buffer, l: usize) -> usize {
    b.line_len(l.min(b.line_count() - 1))
}

fn lend(b: &Buffer, l: usize) -> usize {
    lstart(b, l) + llen(b, l)
}

fn line_of(b: &Buffer, p: usize) -> usize {
    b.char_to_line(p).min(nlines(b) - 1)
}

fn ch(b: &Buffer, i: usize) -> char {
    b.char_at(i).unwrap_or('\n')
}

fn first_nb(b: &Buffer, l: usize) -> usize {
    let n = b.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').count();
    lstart(b, l) + n
}

fn leading_ws(b: &Buffer, l: usize) -> String {
    b.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// The cursor cannot rest on the line break in Normal mode.
fn clamp_normal(b: &Buffer, p: usize) -> usize {
    let p = p.min(b.len_chars());
    let l = line_of(b, p);
    let (s, n) = (lstart(b, l), llen(b, l));
    if n == 0 { s } else { p.min(s + n - 1).max(s) }
}

fn cls(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_fwd(b: &Buffer, from: usize, big: bool) -> usize {
    let n = b.len_chars();
    let mut p = from;
    if p >= n {
        return n;
    }
    let c0 = cls(ch(b, p), big);
    if c0 != 0 {
        while p < n && cls(ch(b, p), big) == c0 {
            p += 1;
        }
    }
    while p < n {
        let c = ch(b, p);
        if c == '\n' {
            if p > from && (p == 0 || ch(b, p - 1) == '\n') {
                break;
            }
            p += 1;
        } else if c.is_whitespace() {
            p += 1;
        } else {
            break;
        }
    }
    p
}

fn word_end(b: &Buffer, from: usize, big: bool) -> usize {
    let n = b.len_chars();
    let mut p = from + 1;
    while p < n && ch(b, p).is_whitespace() {
        p += 1;
    }
    if p >= n {
        return n.saturating_sub(1);
    }
    let c = cls(ch(b, p), big);
    while p + 1 < n && cls(ch(b, p + 1), big) == c {
        p += 1;
    }
    p
}

fn word_back(b: &Buffer, from: usize, big: bool) -> usize {
    if from == 0 {
        return 0;
    }
    let mut p = from - 1;
    while p > 0 && ch(b, p).is_whitespace() {
        if ch(b, p) == '\n' && ch(b, p - 1) == '\n' {
            return p;
        }
        p -= 1;
    }
    if ch(b, p).is_whitespace() {
        return p;
    }
    let c = cls(ch(b, p), big);
    while p > 0 && cls(ch(b, p - 1), big) == c {
        p -= 1;
    }
    p
}

fn word_end_back(b: &Buffer, from: usize, big: bool) -> usize {
    let c = cls(ch(b, from), big);
    let mut q = from;
    if c != 0 {
        while q > 0 && cls(ch(b, q - 1), big) == c {
            q -= 1;
        }
    }
    if q == 0 {
        return 0;
    }
    q -= 1;
    while q > 0 && ch(b, q).is_whitespace() {
        q -= 1;
    }
    q
}

fn at_word_end(b: &Buffer, p: usize, big: bool) -> bool {
    let c = cls(ch(b, p), big);
    c != 0 && (p + 1 >= b.len_chars() || cls(ch(b, p + 1), big) != c)
}

fn find_char(b: &Buffer, from: usize, target: char, fwd: bool, till: bool, n: usize, repeat: bool) -> Option<usize> {
    let l = line_of(b, from);
    let (s, e) = (lstart(b, l), lend(b, l));
    let mut cnt = 0;
    if fwd {
        let mut i = from + 1;
        if till && repeat && i < e && ch(b, i) == target {
            i += 1;
        }
        while i < e {
            if ch(b, i) == target {
                cnt += 1;
                if cnt == n {
                    return Some(if till { i - 1 } else { i });
                }
            }
            i += 1;
        }
    } else {
        let mut i = from;
        if till && repeat && i > s && ch(b, i - 1) == target {
            i -= 1;
        }
        while i > s {
            i -= 1;
            if ch(b, i) == target {
                cnt += 1;
                if cnt == n {
                    return Some(if till { i + 1 } else { i });
                }
            }
        }
    }
    None
}

fn match_pair(b: &Buffer, from: usize) -> Option<usize> {
    let l = line_of(b, from);
    let e = lend(b, l);
    let mut i = from;
    while i < e && !"()[]{}".contains(ch(b, i)) {
        i += 1;
    }
    if i >= e {
        return None;
    }
    let (open, close, fwd) = match ch(b, i) {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        _ => return None,
    };
    let n = b.len_chars();
    let mut depth = 0i64;
    if fwd {
        let mut j = i;
        while j < n {
            let x = ch(b, j);
            if x == open {
                depth += 1;
            } else if x == close {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            j += 1;
        }
    } else {
        let mut j = i as isize;
        while j >= 0 {
            let x = ch(b, j as usize);
            if x == close {
                depth += 1;
            } else if x == open {
                depth -= 1;
                if depth == 0 {
                    return Some(j as usize);
                }
            }
            j -= 1;
        }
    }
    None
}

fn blank(b: &Buffer, l: usize) -> bool {
    b.line(l).trim().is_empty()
}

fn para_fwd(b: &Buffer, from: usize) -> usize {
    let nl = nlines(b);
    let mut l = line_of(b, from);
    while l < nl && blank(b, l) {
        l += 1;
    }
    while l < nl && !blank(b, l) {
        l += 1;
    }
    if l >= nl { b.len_chars() } else { lstart(b, l) }
}

fn para_back(b: &Buffer, from: usize) -> usize {
    let mut l = line_of(b, from) as isize;
    while l >= 0 && blank(b, l as usize) {
        l -= 1;
    }
    while l >= 0 && !blank(b, l as usize) {
        l -= 1;
    }
    if l < 0 { 0 } else { lstart(b, l as usize) }
}

fn make_re(pat: &str) -> Option<Regex> {
    if pat.is_empty() {
        return None;
    }
    Regex::new(pat).ok().or_else(|| Regex::new(&regex::escape(pat)).ok())
}

/// Next match of `re` after (or, backwards, before) `from`, wrapping around the document.
fn search(b: &Buffer, re: &Regex, from: usize, fwd: bool) -> Option<usize> {
    let nl = nlines(b);
    let l0 = line_of(b, from);
    let c0 = from.saturating_sub(lstart(b, l0));
    let cols = |l: usize| -> Vec<usize> {
        let t = b.line(l);
        re.find_iter(&t).map(|m| t[..m.start()].chars().count()).collect()
    };
    for i in 0..=nl {
        let (l, hit) = if fwd {
            let l = (l0 + i) % nl;
            let cs = cols(l);
            let hit = if i == 0 {
                cs.into_iter().find(|&c| c > c0)
            } else if i == nl {
                cs.into_iter().find(|&c| c <= c0)
            } else {
                cs.into_iter().next()
            };
            (l, hit)
        } else {
            let l = (l0 + nl - i) % nl;
            let cs = cols(l);
            let hit = if i == 0 {
                cs.into_iter().rev().find(|&c| c < c0)
            } else if i == nl {
                cs.into_iter().rev().find(|&c| c >= c0)
            } else {
                cs.into_iter().next_back()
            };
            (l, hit)
        };
        if let Some(c) = hit {
            return Some(lstart(b, l) + c);
        }
    }
    None
}

/// The word under (or after) `from` on its line.
fn word_at(b: &Buffer, from: usize) -> Option<(usize, usize)> {
    let l = line_of(b, from);
    let (s, e) = (lstart(b, l), lend(b, l));
    let mut p = from;
    while p < e && cls(ch(b, p), false) != 1 {
        p += 1;
    }
    if p >= e {
        return None;
    }
    let mut a = p;
    while a > s && cls(ch(b, a - 1), false) == 1 {
        a -= 1;
    }
    let mut z = p;
    while z < e && cls(ch(b, z), false) == 1 {
        z += 1;
    }
    Some((a, z))
}

// ───────────── local text objects ─────────────

fn obj_word(b: &Buffer, pos: usize, inner: bool, big: bool) -> Option<(usize, usize)> {
    let l = line_of(b, pos);
    let (ls, le) = (lstart(b, l), lend(b, l));
    if ls == le {
        return None;
    }
    let p = pos.min(le - 1);
    let c = cls(ch(b, p), big);
    let mut a = p;
    while a > ls && cls(ch(b, a - 1), big) == c {
        a -= 1;
    }
    let mut e = p + 1;
    while e < le && cls(ch(b, e), big) == c {
        e += 1;
    }
    if inner {
        return Some((a, e));
    }
    if c == 0 {
        let mut z = e;
        if z < le {
            let c2 = cls(ch(b, z), big);
            while z < le && cls(ch(b, z), big) == c2 {
                z += 1;
            }
        }
        return Some((a, z));
    }
    let mut z = e;
    while z < le && cls(ch(b, z), big) == 0 {
        z += 1;
    }
    if z > e {
        return Some((a, z));
    }
    let mut s = a;
    while s > ls && cls(ch(b, s - 1), big) == 0 {
        s -= 1;
    }
    Some((s, e))
}

fn obj_pair(b: &Buffer, pos: usize, open: char, close: char, inner: bool) -> Option<(usize, usize)> {
    let n = b.len_chars();
    let o = if ch(b, pos) == open {
        pos
    } else {
        let mut depth = 0i64;
        let mut j = pos as isize - 1;
        let mut found = None;
        while j >= 0 {
            let x = ch(b, j as usize);
            if x == close {
                depth += 1;
            } else if x == open {
                if depth == 0 {
                    found = Some(j as usize);
                    break;
                }
                depth -= 1;
            }
            j -= 1;
        }
        found?
    };
    let mut depth = 0i64;
    let mut c = None;
    let mut j = o + 1;
    while j < n {
        let x = ch(b, j);
        if x == open {
            depth += 1;
        } else if x == close {
            if depth == 0 {
                c = Some(j);
                break;
            }
            depth -= 1;
        }
        j += 1;
    }
    let c = c?;
    if !inner {
        return Some((o, c + 1));
    }
    let (mut a, mut z) = (o + 1, c);
    // A block spread over lines: the inner text is the whole lines between the brackets.
    if ch(b, a) == '\n' && a < z {
        let lc = line_of(b, c);
        let before = b.slice(lstart(b, lc), c);
        a += 1;
        if before.trim().is_empty() {
            z = lstart(b, lc);
        }
        if z < a {
            z = a;
        }
    }
    Some((a, z))
}

fn obj_quote(b: &Buffer, pos: usize, q: char, inner: bool) -> Option<(usize, usize)> {
    let l = line_of(b, pos);
    let (ls, le) = (lstart(b, l), lend(b, l));
    let mut qs = Vec::new();
    let mut i = ls;
    while i < le {
        let x = ch(b, i);
        if x == '\\' {
            i += 2;
            continue;
        }
        if x == q {
            qs.push(i);
        }
        i += 1;
    }
    let pair = qs
        .chunks(2)
        .filter(|c| c.len() == 2)
        .find(|c| c[0] <= pos && pos <= c[1])
        .or_else(|| qs.chunks(2).filter(|c| c.len() == 2).find(|c| c[0] > pos))?;
    let (a, z) = (pair[0], pair[1]);
    if inner {
        return Some((a + 1, z));
    }
    let mut e = z + 1;
    while e < le && matches!(ch(b, e), ' ' | '\t') {
        e += 1;
    }
    Some((a, e))
}

fn obj_para(b: &Buffer, pos: usize, inner: bool) -> Option<(usize, usize)> {
    let nl = nlines(b);
    let l = line_of(b, pos);
    let kind = blank(b, l);
    let (mut l0, mut l1) = (l, l);
    while l0 > 0 && blank(b, l0 - 1) == kind {
        l0 -= 1;
    }
    while l1 + 1 < nl && blank(b, l1 + 1) == kind {
        l1 += 1;
    }
    if !inner {
        if l1 + 1 < nl {
            l1 += 1;
            while l1 + 1 < nl && blank(b, l1 + 1) != kind {
                l1 += 1;
            }
        } else if l0 > 0 {
            l0 -= 1;
            while l0 > 0 && blank(b, l0 - 1) != kind {
                l0 -= 1;
            }
        }
    }
    let a = lstart(b, l0);
    let z = if l1 + 1 < b.line_count() { lstart(b, l1 + 1) } else { b.len_chars() };
    Some((a, z))
}

/// The local word / paragraph / bracket / quote text objects.
pub fn local_object(b: &Buffer, pos: usize, kind: char, inner: bool) -> Option<(usize, usize)> {
    match kind {
        'w' => obj_word(b, pos, inner, false),
        'W' => obj_word(b, pos, inner, true),
        'p' => obj_para(b, pos, inner),
        '(' | ')' | 'b' => obj_pair(b, pos, '(', ')', inner),
        '[' | ']' => obj_pair(b, pos, '[', ']', inner),
        '{' | '}' | 'B' => obj_pair(b, pos, '{', '}', inner),
        '<' | '>' => obj_pair(b, pos, '<', '>', inner),
        '"' | '\'' | '`' => obj_quote(b, pos, kind, inner),
        _ => None,
    }
}

// ───────────── command grammar ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    Toggle,
}

#[derive(Clone, PartialEq, Debug)]
enum Mo {
    Left,
    Right,
    Down,
    Up,
    WordF(bool),
    /// `cw` on a word: changes to its end.
    CW(bool),
    WordB(bool),
    WordE(bool),
    WordEBack(bool),
    Home,
    FirstNb,
    End,
    DocStart,
    DocEnd,
    Find(char, bool, bool),
    RepFind(bool),
    Match,
    ParaF,
    ParaB,
    NextLine,
    PrevLine,
    Under,
    Search(bool),
    Star(bool),
    Mark(char, bool),
    Col,
}

#[derive(Clone, PartialEq, Debug)]
enum Tgt {
    Mo(Mo),
    Lines,
    Obj(char, bool),
}

#[derive(Clone, PartialEq, Debug)]
enum Cmd {
    Move(Mo),
    Op(Op, Tgt),
    Act(char),
    ActArg(char, char),
    Vis(char),
    ObjSel(char, bool),
}

#[derive(Clone, PartialEq, Debug)]
struct Parsed {
    reg: Option<char>,
    count: Option<usize>,
    cmd: Cmd,
}

enum P<T> {
    Inc,
    No,
    Yes(T),
}

const CTL: u32 = 0xE000;

fn ctl(c: char) -> char {
    char::from_u32(CTL + c as u32).unwrap_or('\0')
}

fn to_ch(k: VKey) -> char {
    match k {
        VKey::Char(c) => c,
        VKey::Esc => '\u{1b}',
        VKey::Enter => '\r',
        VKey::Backspace => '\u{8}',
        VKey::Ctrl(c) => ctl(c),
    }
}

fn parse_count(k: &[char], mut i: usize) -> (Option<usize>, usize) {
    let mut n: Option<usize> = None;
    while let Some(&c) = k.get(i) {
        match c.to_digit(10) {
            Some(d) if d != 0 || n.is_some() => {
                n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(1_000_000));
                i += 1;
            }
            _ => break,
        }
    }
    (n, i)
}

fn parse_motion(k: &[char]) -> P<Mo> {
    let Some(&c) = k.first() else { return P::Inc };
    let y = P::Yes;
    match c {
        'h' | '\u{8}' => y(Mo::Left),
        'l' | ' ' => y(Mo::Right),
        'j' => y(Mo::Down),
        'k' => y(Mo::Up),
        'w' => y(Mo::WordF(false)),
        'W' => y(Mo::WordF(true)),
        'b' => y(Mo::WordB(false)),
        'B' => y(Mo::WordB(true)),
        'e' => y(Mo::WordE(false)),
        'E' => y(Mo::WordE(true)),
        '0' => y(Mo::Home),
        '^' => y(Mo::FirstNb),
        '$' => y(Mo::End),
        'G' => y(Mo::DocEnd),
        '%' => y(Mo::Match),
        '{' => y(Mo::ParaB),
        '}' => y(Mo::ParaF),
        ';' => y(Mo::RepFind(false)),
        ',' => y(Mo::RepFind(true)),
        '+' | '\r' => y(Mo::NextLine),
        '-' => y(Mo::PrevLine),
        '_' => y(Mo::Under),
        'n' => y(Mo::Search(true)),
        'N' => y(Mo::Search(false)),
        '*' => y(Mo::Star(true)),
        '#' => y(Mo::Star(false)),
        '|' => y(Mo::Col),
        'f' | 'F' | 't' | 'T' => match k.get(1) {
            None => P::Inc,
            Some(&x) => y(Mo::Find(x, c == 'f' || c == 't', c == 't' || c == 'T')),
        },
        '\'' | '`' => match k.get(1) {
            None => P::Inc,
            Some(&x) => y(Mo::Mark(x, c == '`')),
        },
        'g' => match k.get(1) {
            None => P::Inc,
            Some('g') => y(Mo::DocStart),
            Some('e') => y(Mo::WordEBack(false)),
            Some('E') => y(Mo::WordEBack(true)),
            Some('j') => y(Mo::Down),
            Some('k') => y(Mo::Up),
            Some(_) => P::No,
        },
        _ => P::No,
    }
}

fn op_of(k: &[char]) -> Option<(Op, usize)> {
    match *k.first()? {
        'd' => Some((Op::Delete, 1)),
        'c' => Some((Op::Change, 1)),
        'y' => Some((Op::Yank, 1)),
        '>' => Some((Op::Indent, 1)),
        '<' => Some((Op::Outdent, 1)),
        'g' => match k.get(1)? {
            'u' => Some((Op::Lower, 2)),
            'U' => Some((Op::Upper, 2)),
            '~' => Some((Op::Toggle, 2)),
            _ => None,
        },
        _ => None,
    }
}

/// Parse the keys typed so far (without Esc). `visual`: a selection exists; `rec`: `q` stops a
/// recording.
fn parse(k: &[char], visual: bool, rec: bool) -> P<Parsed> {
    let mut i = 0;
    let mut reg = None;
    if k.first() == Some(&'"') {
        match k.get(1) {
            None => return P::Inc,
            Some(&r) => {
                reg = Some(r);
                i = 2;
            }
        }
    }
    let (count, ni) = parse_count(k, i);
    i = ni;
    let Some(&c) = k.get(i) else { return P::Inc };
    let rest = &k[i..];
    let done = |cmd: Cmd, count: Option<usize>| P::Yes(Parsed { reg, count, cmd });

    if !visual && let Some((op, len)) = op_of(rest) {
        let (c2, j) = parse_count(rest, len);
        let count = match (count, c2) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1)).min(1_000_000)),
        };
        let after = &rest[j..];
        let Some(&f) = after.first() else { return P::Inc };
        let key = rest[len - 1];
        if f == key && len == 1 {
            return done(Cmd::Op(op, Tgt::Lines), count);
        }
        if len == 2 {
            if f == key {
                return done(Cmd::Op(op, Tgt::Lines), count);
            }
            if f == 'g' {
                match after.get(1) {
                    None => return P::Inc,
                    Some(&x) if x == key => return done(Cmd::Op(op, Tgt::Lines), count),
                    _ => {}
                }
            }
        }
        if f == 'i' || f == 'a' {
            return match after.get(1) {
                None => P::Inc,
                Some(&x) => done(Cmd::Op(op, Tgt::Obj(x, f == 'i')), count),
            };
        }
        return match parse_motion(after) {
            P::Inc => P::Inc,
            P::No => P::No,
            P::Yes(m) => done(Cmd::Op(op, Tgt::Mo(m)), count),
        };
    }
    if visual {
        if (c == 'i' || c == 'a') && rest.len() >= 2 {
            return done(Cmd::ObjSel(rest[1], c == 'i'), count);
        }
        if c == 'i' || c == 'a' {
            return P::Inc;
        }
        if c == 'g' {
            match rest.get(1) {
                None => return P::Inc,
                Some('u') => return done(Cmd::Vis('u'), count),
                Some('U') => return done(Cmd::Vis('U'), count),
                Some('~') => return done(Cmd::Vis('~'), count),
                _ => {}
            }
        }
        if c == 'r' {
            return match rest.get(1) {
                None => P::Inc,
                Some(&x) => done(Cmd::ActArg('r', x), count),
            };
        }
        if "dxXcsSyYDCRJuU~pPoO><vV:".contains(c) {
            return done(Cmd::Vis(c), count);
        }
    }
    match parse_motion(rest) {
        P::Yes(m) => return done(Cmd::Move(m), count),
        P::Inc => return P::Inc,
        P::No => {}
    }
    match c {
        'r' | 'm' | '@' => match rest.get(1) {
            None => P::Inc,
            Some(&x) => done(Cmd::ActArg(c, x), count),
        },
        'q' if rec => done(Cmd::Act('q'), count),
        'q' => match rest.get(1) {
            None => P::Inc,
            Some(&x) => done(Cmd::ActArg('q', x), count),
        },
        'x' | 'X' | 's' | 'S' | 'D' | 'C' | 'Y' | 'p' | 'P' | 'J' | 'u' | '.' | 'i' | 'a' | 'I' | 'A' | 'o' | 'O'
        | 'v' | 'V' | '~' | ':' | '/' | '?' => done(Cmd::Act(c), count),
        _ if c == ctl('r') || c == ctl('d') || c == ctl('u') => done(Cmd::Act(c), count),
        _ => P::No,
    }
}

// ───────────── motion results and ranges ─────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Excl,
    Incl,
    Line,
}

struct Mr {
    to: usize,
    kind: K,
}

impl Mr {
    fn ex(to: usize) -> Mr {
        Mr { to, kind: K::Excl }
    }
    fn inc(to: usize) -> Mr {
        Mr { to, kind: K::Incl }
    }
    fn line(to: usize) -> Mr {
        Mr { to, kind: K::Line }
    }
}

/// A range to operate on: chars `[a, b)`, or whole lines `l0..=l1` (then `a`/`b` span them).
struct Rg {
    a: usize,
    b: usize,
    lines: Option<(usize, usize)>,
}

fn line_span(b: &Buffer, l0: usize, l1: usize) -> (usize, usize) {
    let n = b.len_chars();
    if l1 + 1 < b.line_count() {
        (lstart(b, l0), lstart(b, l1 + 1))
    } else if l0 > 0 {
        (lend(b, l0 - 1), n.max(lend(b, l1)))
    } else {
        (0, n)
    }
}

fn rg_lines(b: &Buffer, l0: usize, l1: usize) -> Rg {
    let (a, z) = line_span(b, l0, l1);
    Rg { a, b: z, lines: Some((l0, l1)) }
}

fn lines_text(b: &Buffer, l0: usize, l1: usize) -> String {
    let mut s = String::new();
    for l in l0..=l1 {
        s.push_str(&b.line(l));
        s.push('\n');
    }
    s
}

fn toggle_case(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<String>()
            } else if c.is_uppercase() {
                c.to_lowercase().collect::<String>()
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn split_delim(s: &str, d: char) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c != d {
                out.last_mut().unwrap().push('\\');
            }
            out.last_mut().unwrap().push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == d {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    if esc {
        out.last_mut().unwrap().push('\\');
    }
    out
}

/// Vim replacement text (`\1`, `&`) as a `regex` replacement (`${1}`, `${0}`).
fn conv_rep(r: &str) -> String {
    let mut out = String::new();
    let mut it = r.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(d);
                    out.push('}');
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('$') => out.push_str("$$"),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            '&' => out.push_str("${0}"),
            '$' => out.push_str("$$"),
            o => out.push(o),
        }
    }
    out
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

impl Vim {
    pub fn new() -> Vim {
        Vim {
            mode: Mode::Normal,
            objects: None,
            macro_regs: HashMap::new(),
            macro_rec: None,
            last_macro: None,
            pending: Vec::new(),
            cur: Vec::new(),
            cmdline: None,
            regs: HashMap::new(),
            last_mirror: String::new(),
            find: None,
            search: None,
            marks: HashMap::new(),
            want: None,
            vanchor: 0,
            vcursor: 0,
            last_change: Vec::new(),
            recording_change: false,
            replaying: false,
            dirty: false,
            ins_keys: Vec::new(),
            ins_repeat: 0,
            ins_open: false,
            in_group: false,
            gen_id: u64::MAX,
        }
    }

    /// Install a text-object provider (tree-sitter functions, classes, arguments, ...); it is asked
    /// before the local fallbacks.
    #[allow(dead_code)]
    pub fn set_text_objects(&mut self, provider: Box<dyn TextObjects>) {
        self.objects = Some(provider);
    }

    /// Text for the status bar: the command line being typed, the pending keys, or the mode.
    pub fn status(&self) -> String {
        let rec = self.macro_rec.map(|r| format!("  recording @{r}")).unwrap_or_default();
        if let Some((p, s)) = &self.cmdline {
            return format!("{p}{s}");
        }
        let mode = match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        };
        let pend: String = self.pending.iter().map(|k| to_ch(*k)).filter(|c| !c.is_control()).collect();
        if pend.is_empty() { format!("{mode}{rec}") } else { format!("{mode}  {pend}{rec}") }
    }

    /// A key typed in Insert mode that the editor handles itself: remember it for `.` and `3i`.
    fn log_insert(&mut self, k: VKey) {
        if self.mode == Mode::Insert && matches!(k, VKey::Char(_) | VKey::Enter | VKey::Backspace) {
            self.ins_keys.push(k);
            if self.recording_change {
                self.last_change.push(k);
            }
        }
    }

    /// After the app changed the text under the engine (undo, redo): collapse the selection and
    /// keep the cursor on a character.
    pub fn settle(&mut self, buf: &Buffer, sel: &mut Sel) {
        if self.mode == Mode::Normal {
            *sel = Sel::caret(clamp_normal(buf, sel.head));
        }
    }

    /// Switch to Normal mode without a key (the setting was turned off, another document came up).
    pub fn reset(&mut self) {
        self.mode = Mode::Normal;
        self.pending.clear();
        self.cur.clear();
        self.cmdline = None;
        self.in_group = false;
        self.recording_change = false;
        self.want = None;
    }

    pub fn feed(&mut self, k: VKey, cx: &mut Ctx) -> Feed {
        if cx.buf.gen_id() != self.gen_id {
            self.gen_id = cx.buf.gen_id();
            self.reset();
            self.marks.clear();
        }
        if self.mode == Mode::Normal && !cx.sel.is_empty() && self.cmdline.is_none() && self.pending.is_empty() {
            self.vanchor = if cx.sel.anchor > cx.sel.head { cx.sel.anchor - 1 } else { cx.sel.anchor };
            self.vcursor = if cx.sel.head > cx.sel.anchor { cx.sel.head - 1 } else { cx.sel.head };
            self.mode = Mode::Visual;
        }
        let r = if self.mode == Mode::Insert { self.insert_key(k, cx) } else { self.command_key(k, cx) };
        self.sync_sel(cx);
        r
    }

    fn sync_sel(&mut self, cx: &mut Ctx) {
        match self.mode {
            Mode::Visual => {
                let (a, h) = (self.vanchor.min(cx.buf.len_chars()), self.vcursor.min(cx.buf.len_chars()));
                *cx.sel = if h >= a {
                    Sel { anchor: a, head: if ch(cx.buf, h) == '\n' { h } else { (h + 1).min(cx.buf.len_chars()) } }
                } else {
                    Sel { anchor: if ch(cx.buf, a) == '\n' { a } else { (a + 1).min(cx.buf.len_chars()) }, head: h }
                };
            }
            Mode::VisualLine => {
                let (l0, l1) = self.vis_lines(cx.buf);
                *cx.sel = if self.vcursor >= self.vanchor {
                    Sel { anchor: lstart(cx.buf, l0), head: lend(cx.buf, l1) }
                } else {
                    Sel { anchor: lend(cx.buf, l1), head: lstart(cx.buf, l0) }
                };
            }
            _ => {}
        }
    }

    fn vis_lines(&self, b: &Buffer) -> (usize, usize) {
        let (x, y) = (line_of(b, self.vanchor), line_of(b, self.vcursor));
        (x.min(y), x.max(y))
    }

    // ───────────── undo grouping and edits ─────────────

    fn begin_group(&mut self, cx: &mut Ctx) {
        if !self.in_group {
            cx.buf.begin_compound(*cx.sel);
            self.in_group = true;
        }
    }

    fn end_group(&mut self, cx: &mut Ctx) {
        if self.in_group {
            cx.buf.end_compound(*cx.sel);
            self.in_group = false;
        }
    }

    fn edit(&mut self, cx: &mut Ctx, a: usize, z: usize, text: &str) {
        self.begin_group(cx);
        self.dirty = true;
        let t = text.replace('\n', cx.buf.eol());
        let before = *cx.sel;
        cx.buf.replace(a, z, &t, EditKind::Other, before);
    }

    fn set_cursor(&mut self, cx: &mut Ctx, p: usize) {
        *cx.sel = Sel::caret(p.min(cx.buf.len_chars()));
    }

    // ───────────── registers ─────────────

    fn set_reg(&mut self, cx: &mut Ctx, reg: Option<char>, text: String, lines: bool, yank: bool) {
        if reg == Some('_') {
            return;
        }
        let text = text.replace("\r\n", "\n");
        match reg {
            Some(c) if c.is_ascii_uppercase() => {
                let key = c.to_ascii_lowercase();
                let e = self.regs.entry(key).or_default();
                if lines && !e.text.is_empty() && !e.text.ends_with('\n') {
                    e.text.push('\n');
                }
                e.text.push_str(&text);
                e.lines = e.lines || lines;
            }
            Some('+') | Some('*') => cx.clip.set(&text),
            Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {
                self.regs.insert(c, Reg { text: text.clone(), lines });
            }
            _ => {}
        }
        self.regs.insert('"', Reg { text: text.clone(), lines });
        if yank {
            self.regs.insert('0', Reg { text: text.clone(), lines });
        }
        cx.clip.set(&text);
        self.last_mirror = text;
    }

    fn get_reg(&mut self, cx: &mut Ctx, reg: Option<char>) -> Option<Reg> {
        let from_sys = |s: String| {
            let s = s.replace("\r\n", "\n");
            let lines = s.ends_with('\n');
            Reg { text: s, lines }
        };
        match reg {
            Some('+') | Some('*') => Some(from_sys(cx.clip.get())).filter(|r| !r.text.is_empty()),
            None | Some('"') => {
                let sys = cx.clip.get();
                if !sys.is_empty() && sys != self.last_mirror {
                    return Some(from_sys(sys));
                }
                self.regs.get(&'"').cloned().filter(|r| !r.text.is_empty())
            }
            Some(c) => self.regs.get(&c.to_ascii_lowercase()).cloned().filter(|r| !r.text.is_empty()),
        }
    }

    // ───────────── insert mode ─────────────

    fn start_insert(&mut self, cx: &mut Ctx, repeat: usize, open: bool) {
        self.begin_group(cx);
        self.mode = Mode::Insert;
        self.ins_keys.clear();
        self.ins_repeat = repeat;
        self.ins_open = open;
        self.dirty = true;
    }

    fn insert_key(&mut self, k: VKey, cx: &mut Ctx) -> Feed {
        if matches!(k, VKey::Esc | VKey::Ctrl('[')) {
            self.leave_insert(cx);
            return Feed::Done;
        }
        self.log_insert(k);
        if self.replaying {
            self.raw_insert(k, cx);
            return Feed::Done;
        }
        Feed::Pass
    }

    fn raw_insert(&mut self, k: VKey, cx: &mut Ctx) {
        let h = cx.sel.head;
        match k {
            VKey::Char(c) => {
                self.edit(cx, h, h, &c.to_string());
                self.set_cursor(cx, h + 1);
            }
            VKey::Enter => {
                let ind = leading_ws(cx.buf, line_of(cx.buf, h));
                let t = format!("\n{ind}");
                self.edit(cx, h, h, &t);
                self.set_cursor(cx, h + 1 + ind.chars().count());
            }
            VKey::Backspace if h > 0 => {
                self.edit(cx, h - 1, h, "");
                self.set_cursor(cx, h - 1);
            }
            _ => {}
        }
    }

    fn leave_insert(&mut self, cx: &mut Ctx) {
        let keys = std::mem::take(&mut self.ins_keys);
        let (repeat, open) = (self.ins_repeat, self.ins_open);
        self.ins_repeat = 0;
        self.ins_open = false;
        if repeat > 1 {
            for _ in 1..repeat {
                if open {
                    let l = line_of(cx.buf, cx.sel.head);
                    let e = lend(cx.buf, l);
                    let ind = leading_ws(cx.buf, l);
                    let t = format!("\n{ind}");
                    self.edit(cx, e, e, &t);
                    self.set_cursor(cx, e + 1 + ind.chars().count());
                }
                for k in &keys {
                    self.raw_insert(*k, cx);
                }
            }
        }
        if self.recording_change {
            self.last_change.push(VKey::Esc);
            self.recording_change = false;
        }
        self.end_group(cx);
        self.mode = Mode::Normal;
        let h = cx.sel.head;
        if h > lstart(cx.buf, line_of(cx.buf, h)) {
            self.set_cursor(cx, h - 1);
        }
        self.want = None;
    }

    // ───────────── normal / visual mode ─────────────

    fn command_key(&mut self, k: VKey, cx: &mut Ctx) -> Feed {
        if self.cmdline.is_some() {
            let f = self.cmdline_key(k, cx);
            self.finish(false, cx);
            return f;
        }
        if matches!(k, VKey::Esc | VKey::Ctrl('[')) {
            self.pending.clear();
            self.cur.clear();
            if self.mode != Mode::Normal {
                self.leave_visual(cx);
            }
            return Feed::Done;
        }
        self.pending.push(k);
        self.cur.push(k);
        let chars: Vec<char> = self.pending.iter().map(|k| to_ch(*k)).collect();
        let visual = self.mode != Mode::Normal;
        match parse(&chars, visual, self.macro_rec.is_some()) {
            P::Inc => Feed::Done,
            P::No => {
                self.pending.clear();
                self.cur.clear();
                Feed::Done
            }
            P::Yes(p) => {
                self.pending.clear();
                self.dirty = false;
                let f = self.exec(p, cx);
                self.finish(visual, cx);
                f
            }
        }
    }

    fn finish(&mut self, was_visual: bool, cx: &mut Ctx) {
        if self.mode != Mode::Insert {
            self.end_group(cx);
        }
        if self.dirty && !self.replaying && !was_visual && !self.cur.is_empty() {
            self.last_change = self.cur.clone();
            self.recording_change = self.mode == Mode::Insert;
        }
        self.cur.clear();
        self.dirty = false;
    }

    fn leave_visual(&mut self, cx: &mut Ctx) {
        let (a, z) = (self.vanchor.min(self.vcursor), self.vanchor.max(self.vcursor));
        self.marks.insert('<', a);
        self.marks.insert('>', z);
        self.mode = Mode::Normal;
        let p = clamp_normal(cx.buf, self.vcursor);
        self.set_cursor(cx, p);
    }

    fn exec(&mut self, p: Parsed, cx: &mut Ctx) -> Feed {
        if !matches!(p.cmd, Cmd::Move(Mo::Down | Mo::Up)) {
            self.want = None;
        }
        match p.cmd.clone() {
            Cmd::Move(mo) => {
                self.do_move(&mo, p.count, cx);
                Feed::Done
            }
            Cmd::Op(op, tgt) => self.exec_op(op, tgt, p.reg, p.count, cx),
            Cmd::Vis(c) => self.exec_visual(c, &p, cx),
            Cmd::ObjSel(c, inner) => {
                let pos = self.vcursor;
                if let Some((a, z)) = self.object(cx.buf, pos, c, inner).filter(|(a, z)| z > a) {
                    self.vanchor = a;
                    self.vcursor = z - 1;
                }
                Feed::Done
            }
            Cmd::Act(c) => self.exec_act(c, &p, cx),
            Cmd::ActArg(c, a) => self.exec_actarg(c, a, &p, cx),
        }
    }

    fn object(&self, b: &Buffer, pos: usize, kind: char, inner: bool) -> Option<(usize, usize)> {
        self.objects.as_ref().and_then(|o| o.object(b, pos, kind, inner)).or_else(|| local_object(b, pos, kind, inner))
    }

    /// Run a motion as a cursor movement. False when it failed.
    fn do_move(&mut self, mo: &Mo, count: Option<usize>, cx: &mut Ctx) -> bool {
        let visual = self.mode != Mode::Normal;
        let from = if visual { self.vcursor } else { cx.sel.head };
        let Some(r) = self.motion(mo, count, from, cx, false) else { return false };
        if matches!(mo, Mo::DocStart | Mo::DocEnd | Mo::Search(_) | Mo::Star(_) | Mo::Match) {
            self.marks.insert('\'', from);
        }
        let to = clamp_normal(cx.buf, r.to);
        if visual {
            self.vcursor = to;
        } else {
            self.set_cursor(cx, to);
        }
        true
    }

    fn motion(&mut self, mo: &Mo, count: Option<usize>, from: usize, cx: &Ctx, op: bool) -> Option<Mr> {
        let b: &Buffer = &*cx.buf;
        let n = count.unwrap_or(1).max(1);
        let last = nlines(b) - 1;
        let l = line_of(b, from);
        let col = from.saturating_sub(lstart(b, l));
        match mo {
            Mo::Left => {
                if col == 0 {
                    return None;
                }
                Some(Mr::ex(from - n.min(col)))
            }
            Mo::Right => {
                let e = lend(b, l);
                if from >= e {
                    return None;
                }
                Some(Mr::ex((from + n).min(e)))
            }
            Mo::Down | Mo::Up => {
                let tl = if *mo == Mo::Down {
                    if l >= last {
                        return None;
                    }
                    (l + n).min(last)
                } else {
                    if l == 0 {
                        return None;
                    }
                    l.saturating_sub(n)
                };
                let w = *self.want.get_or_insert(col);
                Some(Mr::line(lstart(b, tl) + w.min(llen(b, tl))))
            }
            Mo::WordF(big) => {
                let mut p = from;
                for i in 0..n {
                    let next = word_fwd(b, p, *big);
                    if op && i == n - 1 {
                        let lp = line_of(b, p);
                        if line_of(b, next.min(b.len_chars())) > lp && next < b.len_chars() {
                            return Some(Mr::ex(lend(b, lp)));
                        }
                    }
                    p = next;
                    if p >= b.len_chars() {
                        break;
                    }
                }
                (p != from).then(|| Mr::ex(p))
            }
            Mo::CW(big) => {
                let mut p = from;
                for i in 0..n {
                    p = if i == 0 && at_word_end(b, p, *big) { p } else { word_end(b, p, *big) };
                }
                Some(Mr::inc(p))
            }
            Mo::WordB(big) => {
                let mut p = from;
                for _ in 0..n {
                    p = word_back(b, p, *big);
                }
                (p != from).then(|| Mr::ex(p))
            }
            Mo::WordE(big) => {
                let mut p = from;
                for _ in 0..n {
                    p = word_end(b, p, *big);
                }
                (p != from).then(|| Mr::inc(p))
            }
            Mo::WordEBack(big) => {
                let mut p = from;
                for _ in 0..n {
                    p = word_end_back(b, p, *big);
                }
                (p != from).then(|| Mr::inc(p))
            }
            Mo::Home => Some(Mr::ex(lstart(b, l))),
            Mo::FirstNb => Some(Mr::ex(first_nb(b, l))),
            Mo::End => Some(Mr::ex(lend(b, (l + n - 1).min(last)))),
            Mo::DocStart => {
                let t = count.map(|c| c.max(1) - 1).unwrap_or(0).min(last);
                Some(Mr::line(first_nb(b, t)))
            }
            Mo::DocEnd => {
                let t = count.map(|c| c.max(1) - 1).unwrap_or(last).min(last);
                Some(Mr::line(first_nb(b, t)))
            }
            Mo::NextLine => {
                let t = (l + n).min(last);
                (t != l).then(|| Mr::line(first_nb(b, t)))
            }
            Mo::PrevLine => {
                if l == 0 {
                    return None;
                }
                Some(Mr::line(first_nb(b, l.saturating_sub(n))))
            }
            Mo::Under => Some(Mr::line(first_nb(b, (l + n - 1).min(last)))),
            Mo::Find(c, fwd, till) => {
                self.find = Some((*c, *fwd, *till));
                find_char(b, from, *c, *fwd, *till, n, false).map(|p| if *fwd { Mr::inc(p) } else { Mr::ex(p) })
            }
            Mo::RepFind(rev) => {
                let (c, f, t) = self.find?;
                let fwd = f != *rev;
                find_char(b, from, c, fwd, t, n, true).map(|p| if fwd { Mr::inc(p) } else { Mr::ex(p) })
            }
            Mo::Match => match_pair(b, from).map(Mr::inc),
            Mo::ParaF => Some(Mr::ex(para_fwd(b, from))),
            Mo::ParaB => Some(Mr::ex(para_back(b, from))),
            Mo::Search(same) => {
                let (pat, fwd) = self.search.clone()?;
                let dir = if *same { fwd } else { !fwd };
                let re = make_re(&pat)?;
                let mut p = from;
                for _ in 0..n {
                    p = search(b, &re, p, dir)?;
                }
                Some(Mr::ex(p))
            }
            Mo::Star(fwd) => {
                let (a, z) = word_at(b, from)?;
                let word = b.slice(a, z);
                let pat = format!("\\b{}\\b", regex::escape(&word));
                let re = make_re(&pat)?;
                self.search = Some((pat, *fwd));
                let mut p = if *fwd { from } else { a };
                for _ in 0..n {
                    p = search(b, &re, p, *fwd)?;
                }
                Some(Mr::ex(p))
            }
            Mo::Mark(c, exact) => {
                let pos = (*self.marks.get(c)?).min(b.len_chars());
                Some(if *exact { Mr::ex(pos) } else { Mr::line(first_nb(b, line_of(b, pos))) })
            }
            Mo::Col => Some(Mr::ex(lstart(b, l) + (n - 1).min(llen(b, l)))),
        }
    }

    // ───────────── operators ─────────────

    fn build_range(&mut self, op: Op, tgt: &Tgt, count: Option<usize>, from: usize, cx: &Ctx) -> Option<Rg> {
        let b: &Buffer = &*cx.buf;
        match tgt {
            Tgt::Lines => {
                let l = line_of(b, from);
                let n = count.unwrap_or(1).max(1);
                Some(rg_lines(b, l, (l + n - 1).min(nlines(b) - 1)))
            }
            Tgt::Obj(c, inner) => {
                let (a, z) = self.object(b, from, *c, *inner)?;
                if *c == 'p' {
                    let l0 = line_of(b, a);
                    let l1 = line_of(b, z.saturating_sub(1).max(a));
                    Some(rg_lines(b, l0, l1))
                } else {
                    Some(Rg { a, b: z, lines: None })
                }
            }
            Tgt::Mo(mo) => {
                let mo2 = match mo {
                    Mo::WordF(big) if op == Op::Change && cls(ch(b, from), *big) != 0 => Mo::CW(*big),
                    m => m.clone(),
                };
                let r = self.motion(&mo2, count, from, cx, true)?;
                match r.kind {
                    K::Line => {
                        let (x, y) = (line_of(b, from), line_of(b, r.to));
                        Some(rg_lines(b, x.min(y), x.max(y)))
                    }
                    K::Incl => {
                        let (a, e) = (from.min(r.to), from.max(r.to));
                        Some(Rg { a, b: (e + 1).min(b.len_chars()), lines: None })
                    }
                    K::Excl => {
                        let (a, mut e) = (from.min(r.to), from.max(r.to));
                        if e > a {
                            let le = line_of(b, e);
                            let la = line_of(b, a);
                            if le > la && e == lstart(b, le) {
                                let prev = le - 1;
                                e = lend(b, prev);
                                if a <= first_nb(b, la) {
                                    return Some(rg_lines(b, la, prev));
                                }
                            }
                        }
                        Some(Rg { a, b: e, lines: None })
                    }
                }
            }
        }
    }

    fn exec_op(&mut self, op: Op, tgt: Tgt, reg: Option<char>, count: Option<usize>, cx: &mut Ctx) -> Feed {
        let from = cx.sel.head;
        let Some(rg) = self.build_range(op, &tgt, count, from, cx) else { return Feed::Done };
        self.run_op(op, rg, reg, cx);
        Feed::Done
    }

    fn rg_text(&self, cx: &Ctx, rg: &Rg) -> String {
        match rg.lines {
            Some((l0, l1)) => lines_text(cx.buf, l0, l1),
            None => cx.buf.slice(rg.a, rg.b),
        }
    }

    fn run_op(&mut self, op: Op, rg: Rg, reg: Option<char>, cx: &mut Ctx) {
        match op {
            Op::Yank => {
                let t = self.rg_text(cx, &rg);
                self.set_reg(cx, reg, t, rg.lines.is_some(), true);
                let p = match rg.lines {
                    Some((l0, _)) => {
                        let h = cx.sel.head;
                        let (hl, hc) = cx.buf.line_col(h);
                        if hl > l0 { lstart(cx.buf, l0) + hc.min(llen(cx.buf, l0)) } else { h }
                    }
                    None => rg.a,
                };
                let p = clamp_normal(cx.buf, p);
                self.set_cursor(cx, p);
            }
            Op::Delete => {
                let t = self.rg_text(cx, &rg);
                self.set_reg(cx, reg, t, rg.lines.is_some(), false);
                self.edit(cx, rg.a, rg.b, "");
                match rg.lines {
                    Some((l0, _)) => {
                        let l = l0.min(nlines(cx.buf) - 1);
                        let p = first_nb(cx.buf, l);
                        let p = clamp_normal(cx.buf, p);
                        self.set_cursor(cx, p);
                    }
                    None => {
                        let p = clamp_normal(cx.buf, rg.a);
                        self.set_cursor(cx, p);
                    }
                }
            }
            Op::Change => {
                let t = self.rg_text(cx, &rg);
                self.set_reg(cx, reg, t, rg.lines.is_some(), false);
                let (a, z) = match rg.lines {
                    Some((l0, l1)) => (lstart(cx.buf, l0), lend(cx.buf, l1)),
                    None => (rg.a, rg.b),
                };
                self.begin_group(cx);
                self.edit(cx, a, z, "");
                self.set_cursor(cx, a);
                self.start_insert(cx, 1, false);
            }
            Op::Indent | Op::Outdent => {
                let (l0, l1) = rg
                    .lines
                    .unwrap_or_else(|| (line_of(cx.buf, rg.a), line_of(cx.buf, rg.b.saturating_sub(1).max(rg.a))));
                let ind = cx.indent.to_string();
                let w = ind.chars().count().max(1);
                for l in (l0..=l1).rev() {
                    let s = lstart(cx.buf, l);
                    if op == Op::Indent {
                        if llen(cx.buf, l) > 0 {
                            self.edit(cx, s, s, &ind);
                        }
                    } else {
                        let t = cx.buf.line(l);
                        let n =
                            if t.starts_with('\t') { 1 } else { t.chars().take(w).take_while(|c| *c == ' ').count() };
                        if n > 0 {
                            self.edit(cx, s, s + n, "");
                        }
                    }
                }
                let p = first_nb(cx.buf, l0.min(nlines(cx.buf) - 1));
                let p = clamp_normal(cx.buf, p);
                self.set_cursor(cx, p);
            }
            Op::Lower | Op::Upper | Op::Toggle => {
                let t = cx.buf.slice(rg.a, rg.b);
                let new = match op {
                    Op::Lower => t.to_lowercase(),
                    Op::Upper => t.to_uppercase(),
                    _ => toggle_case(&t),
                };
                if new != t {
                    self.edit(cx, rg.a, rg.b, &new);
                }
                let p = clamp_normal(cx.buf, rg.a);
                self.set_cursor(cx, p);
            }
        }
    }

    // ───────────── visual mode commands ─────────────

    fn exec_visual(&mut self, c: char, p: &Parsed, cx: &mut Ctx) -> Feed {
        let by_lines = self.mode == Mode::VisualLine;
        let (a, e) = (self.vanchor.min(self.vcursor), self.vanchor.max(self.vcursor));
        let (l0, l1) = (line_of(cx.buf, a), line_of(cx.buf, e));
        let rg =
            if by_lines { rg_lines(cx.buf, l0, l1) } else { Rg { a, b: (e + 1).min(cx.buf.len_chars()), lines: None } };
        let reg = p.reg;
        match c {
            'd' | 'x' => {
                self.leave_visual(cx);
                self.run_op(Op::Delete, rg, reg, cx);
            }
            'y' => {
                self.leave_visual(cx);
                self.run_op(Op::Yank, rg, reg, cx);
                let q = if by_lines { lstart(cx.buf, l0) } else { a };
                let q = clamp_normal(cx.buf, q);
                self.set_cursor(cx, q);
            }
            'c' | 's' => {
                self.leave_visual(cx);
                self.run_op(Op::Change, rg, reg, cx);
            }
            '>' | '<' => {
                self.leave_visual(cx);
                let op = if c == '>' { Op::Indent } else { Op::Outdent };
                self.run_op(op, rg_lines(cx.buf, l0, l1), reg, cx);
            }
            'u' | 'U' | '~' => {
                self.leave_visual(cx);
                let op = match c {
                    'u' => Op::Lower,
                    'U' => Op::Upper,
                    _ => Op::Toggle,
                };
                self.run_op(op, rg, reg, cx);
            }
            'X' | 'D' => {
                self.leave_visual(cx);
                self.run_op(Op::Delete, rg_lines(cx.buf, l0, l1), reg, cx);
            }
            'Y' => {
                self.leave_visual(cx);
                self.run_op(Op::Yank, rg_lines(cx.buf, l0, l1), reg, cx);
                let q = lstart(cx.buf, l0);
                self.set_cursor(cx, q);
            }
            'C' | 'S' | 'R' => {
                self.leave_visual(cx);
                self.run_op(Op::Change, rg_lines(cx.buf, l0, l1), reg, cx);
            }
            'J' => {
                self.leave_visual(cx);
                let q = lstart(cx.buf, l0);
                self.set_cursor(cx, q);
                self.join(cx, (l1 - l0 + 1).max(2));
            }
            'p' | 'P' => {
                let Some(r) = self.get_reg(cx, reg) else {
                    self.leave_visual(cx);
                    return Feed::Done;
                };
                self.leave_visual(cx);
                let keep = if c == 'P' { Some('_') } else { None };
                let sel_lines = rg.lines;
                self.run_op(Op::Delete, rg, keep, cx);
                if sel_lines.is_some() {
                    // Whole lines gone: the register's lines go where they were.
                    let text = if r.lines { r.text.clone() } else { format!("{}\n", r.text) };
                    let n = cx.buf.len_chars();
                    if l0 >= nlines(cx.buf) && n > 0 {
                        let t = if ch(cx.buf, n - 1) == '\n' {
                            text.clone()
                        } else {
                            format!("\n{}", text.trim_end_matches('\n'))
                        };
                        self.edit(cx, n, n, &t);
                    } else {
                        let s = lstart(cx.buf, l0);
                        self.edit(cx, s, s, &text);
                    }
                    let q = first_nb(cx.buf, l0.min(nlines(cx.buf) - 1));
                    let q = clamp_normal(cx.buf, q);
                    self.set_cursor(cx, q);
                } else {
                    let at = a.min(cx.buf.len_chars());
                    let text = if r.lines { format!("\n{}\n", r.text.trim_end_matches('\n')) } else { r.text.clone() };
                    self.edit(cx, at, at, &text);
                    let end = at + text.chars().count();
                    let q = clamp_normal(cx.buf, end.saturating_sub(1));
                    self.set_cursor(cx, q);
                }
            }
            'o' | 'O' => std::mem::swap(&mut self.vanchor, &mut self.vcursor),
            'v' => {
                if self.mode == Mode::Visual {
                    self.leave_visual(cx);
                } else {
                    self.mode = Mode::Visual;
                }
            }
            'V' => {
                if self.mode == Mode::VisualLine {
                    self.leave_visual(cx);
                } else {
                    self.mode = Mode::VisualLine;
                }
            }
            ':' => {
                self.leave_visual(cx);
                self.cmdline = Some((':', "'<,'>".to_string()));
            }
            _ => {}
        }
        Feed::Done
    }

    // ───────────── normal mode actions ─────────────

    fn synth(&mut self, op: Op, tgt: Tgt, p: &Parsed, cx: &mut Ctx) -> Feed {
        self.exec_op(op, tgt, p.reg, p.count, cx)
    }

    fn exec_act(&mut self, c: char, p: &Parsed, cx: &mut Ctx) -> Feed {
        let n = p.count.unwrap_or(1).max(1);
        let h = cx.sel.head;
        let l = line_of(cx.buf, h);
        match c {
            'i' => self.start_insert(cx, n, false),
            'a' => {
                if llen(cx.buf, l) > 0 {
                    self.set_cursor(cx, h + 1);
                }
                self.start_insert(cx, n, false);
            }
            'I' => {
                let q = first_nb(cx.buf, l);
                self.set_cursor(cx, q);
                self.start_insert(cx, n, false);
            }
            'A' => {
                let q = lend(cx.buf, l);
                self.set_cursor(cx, q);
                self.start_insert(cx, n, false);
            }
            'o' => {
                self.start_insert(cx, n, true);
                let e = lend(cx.buf, l);
                let ind = leading_ws(cx.buf, l);
                let t = format!("\n{ind}");
                self.edit(cx, e, e, &t);
                self.set_cursor(cx, e + 1 + ind.chars().count());
            }
            'O' => {
                self.start_insert(cx, n, true);
                let s = lstart(cx.buf, l);
                let ind = leading_ws(cx.buf, l);
                let t = format!("{ind}\n");
                self.edit(cx, s, s, &t);
                self.set_cursor(cx, s + ind.chars().count());
            }
            'v' | 'V' => {
                self.vanchor = h;
                self.vcursor = h;
                self.mode = if c == 'v' { Mode::Visual } else { Mode::VisualLine };
            }
            'x' => return self.synth(Op::Delete, Tgt::Mo(Mo::Right), p, cx),
            'X' => return self.synth(Op::Delete, Tgt::Mo(Mo::Left), p, cx),
            'D' => return self.synth(Op::Delete, Tgt::Mo(Mo::End), p, cx),
            'C' => return self.synth(Op::Change, Tgt::Mo(Mo::End), p, cx),
            's' => {
                if llen(cx.buf, l) == 0 {
                    self.start_insert(cx, 1, false);
                } else {
                    return self.synth(Op::Change, Tgt::Mo(Mo::Right), p, cx);
                }
            }
            'S' => return self.synth(Op::Change, Tgt::Lines, p, cx),
            'Y' => return self.synth(Op::Yank, Tgt::Lines, p, cx),
            'p' | 'P' => self.put(cx, p.reg, c == 'P', n),
            'J' => self.join(cx, n.max(2)),
            '~' => {
                let e = lend(cx.buf, l);
                if h < e {
                    let z = (h + n).min(e);
                    let t = toggle_case(&cx.buf.slice(h, z));
                    self.edit(cx, h, z, &t);
                    let q = clamp_normal(cx.buf, z);
                    self.set_cursor(cx, q);
                }
            }
            'u' => return Feed::Undo(n),
            '.' => {
                if self.last_change.is_empty() {
                    return Feed::Done;
                }
                let keys = self.last_change.clone();
                let was = self.replaying;
                self.replaying = true;
                for _ in 0..n {
                    for k in &keys {
                        self.feed(*k, cx);
                    }
                }
                self.replaying = was;
                if self.mode == Mode::Insert {
                    self.leave_insert(cx);
                }
            }
            ':' | '/' | '?' => self.cmdline = Some((c, String::new())),
            'q' => {
                if let Some(r) = self.macro_rec.take() {
                    return Feed::MacroStop(r);
                }
            }
            _ if c == ctl('r') => return Feed::Redo(n),
            _ if c == ctl('d') || c == ctl('u') => {
                let half = (cx.page / 2).max(1);
                let mo = if c == ctl('d') { Mo::Down } else { Mo::Up };
                self.do_move(&mo, Some(half), cx);
            }
            _ => {}
        }
        Feed::Done
    }

    fn exec_actarg(&mut self, c: char, a: char, p: &Parsed, cx: &mut Ctx) -> Feed {
        let n = p.count.unwrap_or(1).max(1);
        let h = cx.sel.head;
        match c {
            'r' => {
                if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                    let (s, e) = (self.vanchor.min(self.vcursor), self.vanchor.max(self.vcursor));
                    let (s, e) = if self.mode == Mode::VisualLine {
                        let (l0, l1) = self.vis_lines(cx.buf);
                        (lstart(cx.buf, l0), lend(cx.buf, l1))
                    } else {
                        (s, (e + 1).min(cx.buf.len_chars()))
                    };
                    let t: String =
                        cx.buf.slice(s, e).chars().map(|x| if x == '\n' || x == '\r' { x } else { a }).collect();
                    self.leave_visual(cx);
                    self.edit(cx, s, e, &t);
                    let q = clamp_normal(cx.buf, s);
                    self.set_cursor(cx, q);
                    return Feed::Done;
                }
                let e = lend(cx.buf, line_of(cx.buf, h));
                if h + n > e {
                    return Feed::Done;
                }
                if a == '\r' {
                    self.edit(cx, h, h + n, "\n");
                    self.set_cursor(cx, h + 1);
                } else {
                    let t: String = std::iter::repeat_n(a, n).collect();
                    self.edit(cx, h, h + n, &t);
                    self.set_cursor(cx, h + n - 1);
                }
            }
            'm' => {
                self.marks.insert(a, h);
            }
            'q' => {
                if a.is_ascii_alphanumeric() {
                    self.macro_rec = Some(a);
                    return Feed::MacroStart(a);
                }
            }
            '@' => {
                let r = if a == '@' { self.last_macro } else { Some(a) };
                if let Some(r) = r {
                    self.last_macro = Some(r);
                    return Feed::MacroPlay(r, n);
                }
            }
            _ => {}
        }
        Feed::Done
    }

    fn put(&mut self, cx: &mut Ctx, reg: Option<char>, before: bool, n: usize) {
        let Some(r) = self.get_reg(cx, reg) else { return };
        let h = cx.sel.head;
        let l = line_of(cx.buf, h);
        if r.lines {
            let text = r.text.repeat(n);
            let new_line = if before {
                let s = lstart(cx.buf, l);
                self.edit(cx, s, s, &text);
                l
            } else if l + 1 < cx.buf.line_count() {
                let s = lstart(cx.buf, l + 1);
                self.edit(cx, s, s, &text);
                l + 1
            } else {
                let e = cx.buf.len_chars();
                let t = format!("\n{}", text.trim_end_matches('\n'));
                self.edit(cx, e, e, &t);
                l + 1
            };
            let q = first_nb(cx.buf, new_line.min(nlines(cx.buf) - 1));
            let q = clamp_normal(cx.buf, q);
            self.set_cursor(cx, q);
        } else {
            let at = if before || llen(cx.buf, l) == 0 { h } else { h + 1 };
            let text = r.text.repeat(n);
            self.edit(cx, at, at, &text);
            let q = if text.contains('\n') { at } else { at + text.chars().count() - 1 };
            let q = clamp_normal(cx.buf, q);
            self.set_cursor(cx, q);
        }
    }

    fn join(&mut self, cx: &mut Ctx, n: usize) {
        let mut pos = cx.sel.head;
        for _ in 0..n.saturating_sub(1) {
            let l = line_of(cx.buf, pos);
            if l + 1 >= nlines(cx.buf) {
                break;
            }
            let s = lend(cx.buf, l);
            let next = cx.buf.line(l + 1);
            let ws = next.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let e = lstart(cx.buf, l + 1) + ws;
            let cur = cx.buf.line(l);
            let sep = if next.trim().is_empty()
                || next.trim_start().starts_with(')')
                || cur.is_empty()
                || cur.ends_with(' ')
                || cur.ends_with('\t')
            {
                ""
            } else {
                " "
            };
            self.edit(cx, s, e, sep);
            pos = s;
        }
        let q = clamp_normal(cx.buf, pos);
        self.set_cursor(cx, q);
    }

    // ───────────── command line ─────────────

    fn cmdline_key(&mut self, k: VKey, cx: &mut Ctx) -> Feed {
        let Some((p, mut s)) = self.cmdline.take() else { return Feed::Done };
        match k {
            VKey::Esc | VKey::Ctrl('[') => return Feed::Done,
            VKey::Enter => return self.run_cmdline(p, s, cx),
            VKey::Backspace => {
                if s.pop().is_none() {
                    return Feed::Done;
                }
            }
            VKey::Char(c) => s.push(c),
            VKey::Ctrl('u') => s.clear(),
            VKey::Ctrl(_) => {}
        }
        self.cmdline = Some((p, s));
        Feed::Done
    }

    fn run_cmdline(&mut self, p: char, s: String, cx: &mut Ctx) -> Feed {
        if p == ':' {
            return self.ex(&s, cx);
        }
        let pat = if s.is_empty() { self.search.as_ref().map(|x| x.0.clone()) } else { Some(s) };
        let Some(pat) = pat else { return Feed::Done };
        self.search = Some((pat.clone(), p == '/'));
        if self.do_move(&Mo::Search(true), None, cx) {
            Feed::Done
        } else {
            Feed::Message(format!("pattern not found: {pat}"))
        }
    }

    fn parse_addr<'a>(&self, s: &'a str, cur: usize, last: usize) -> Option<(usize, &'a str)> {
        let s = s.trim_start();
        let mut it = s.chars();
        match it.next()? {
            '.' => Some((cur, &s[1..])),
            '$' => Some((last, &s[1..])),
            '\'' => {
                let m = it.next()?;
                let pos = *self.marks.get(&m)?;
                Some((pos, &s[1 + m.len_utf8()..]))
            }
            c if c.is_ascii_digit() => {
                let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                let n: usize = s[..end].parse().ok()?;
                Some((n.saturating_sub(1).min(last), &s[end..]))
            }
            _ => None,
        }
    }

    fn ex(&mut self, line: &str, cx: &mut Ctx) -> Feed {
        let s = line.trim().trim_start_matches(':').trim_start();
        if s.is_empty() {
            return Feed::Done;
        }
        let last = nlines(cx.buf) - 1;
        let cur = line_of(cx.buf, cx.sel.head);
        // Numbers, `.` and `$` are lines, marks are char positions: `parse_addr_line` gives lines.
        let (range, rest): (Option<(usize, usize)>, &str) = if let Some(r) = s.strip_prefix('%') {
            (Some((0, last)), r)
        } else if let Some((a, r)) = self.parse_addr_line(s, cur, last, cx.buf) {
            if let Some(r2) = r.strip_prefix(',') {
                if let Some((z, r3)) = self.parse_addr_line(r2, cur, last, cx.buf) {
                    (Some((a.min(z), a.max(z))), r3)
                } else {
                    (Some((a, a)), r2)
                }
            } else {
                (Some((a, a)), r)
            }
        } else {
            (None, s)
        };
        let rest = rest.trim_start();
        if rest.is_empty() {
            if let Some((_, z)) = range {
                let q = first_nb(cx.buf, z.min(last));
                let q = clamp_normal(cx.buf, q);
                self.set_cursor(cx, q);
            }
            return Feed::Done;
        }
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        let arg = &rest[name.len()..];
        let bang = arg.starts_with('!');
        let (l0, l1) = range.unwrap_or((cur, cur));
        match name.as_str() {
            "w" | "write" => Feed::Ex(ExCmd::Write),
            "q" | "quit" | "clo" | "close" => Feed::Ex(ExCmd::Quit { force: bang }),
            "wq" | "x" | "xit" | "exit" => Feed::Ex(ExCmd::WriteQuit),
            "e" | "edit" => {
                let path = arg.trim_start_matches('!').trim();
                if path.is_empty() {
                    Feed::Message("e needs a file name".into())
                } else {
                    Feed::Ex(ExCmd::Edit(path.to_string()))
                }
            }
            "noh" | "nohlsearch" => Feed::Done,
            "set" | "se" => Feed::Message("set is not supported: use the settings".into()),
            "d" | "delete" => {
                let rg = rg_lines(cx.buf, l0, l1);
                self.run_op(Op::Delete, rg, None, cx);
                self.end_group(cx);
                Feed::Done
            }
            "y" | "yank" => {
                let rg = rg_lines(cx.buf, l0, l1);
                self.run_op(Op::Yank, rg, None, cx);
                Feed::Done
            }
            "s" | "substitute" => self.substitute(arg, l0, l1, cx),
            other => Feed::Message(format!("not an editor command: {other}")),
        }
    }

    fn parse_addr_line<'a>(&self, s: &'a str, cur: usize, last: usize, b: &Buffer) -> Option<(usize, &'a str)> {
        let t = s.trim_start();
        let is_mark = t.starts_with('\'');
        let (v, r) = self.parse_addr(s, cur, last)?;
        Some((if is_mark { line_of(b, v) } else { v }, r))
    }

    fn substitute(&mut self, arg: &str, l0: usize, l1: usize, cx: &mut Ctx) -> Feed {
        let Some(d) = arg.chars().next().filter(|c| !c.is_alphanumeric() && !c.is_whitespace()) else {
            return Feed::Message("usage: :s/pattern/replacement/[g]".into());
        };
        let parts = split_delim(&arg[d.len_utf8()..], d);
        let pat = parts.first().cloned().unwrap_or_default();
        let rep = conv_rep(parts.get(1).map(String::as_str).unwrap_or(""));
        let flags = parts.get(2).cloned().unwrap_or_default();
        let pat = if flags.contains('i') { format!("(?i){pat}") } else { pat };
        let Some(re) = make_re(&pat) else { return Feed::Message("invalid pattern".into()) };
        let global = flags.contains('g');
        let mut changed = 0usize;
        let mut out = Vec::new();
        for l in l0..=l1 {
            let t = cx.buf.line(l);
            let n = if global {
                re.replace_all(&t, rep.as_str()).into_owned()
            } else {
                re.replace(&t, rep.as_str()).into_owned()
            };
            if n != t {
                changed += 1;
            }
            out.push(n);
        }
        if changed == 0 {
            return Feed::Message(format!("pattern not found: {}", parts.first().map(String::as_str).unwrap_or("")));
        }
        let (a, z) = (lstart(cx.buf, l0), lend(cx.buf, l1));
        let text = out.join("\n");
        self.edit(cx, a, z, &text);
        let end_line = (l0 + text.matches('\n').count()).min(nlines(cx.buf) - 1);
        let q = first_nb(cx.buf, end_line);
        let q = clamp_normal(cx.buf, q);
        self.set_cursor(cx, q);
        self.end_group(cx);
        Feed::Message(format!("{changed} line(s) changed"))
    }
}

#[cfg(test)]
mod tests;
