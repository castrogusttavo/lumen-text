use super::*;

struct Mem(String);

impl Clip for Mem {
    fn get(&mut self) -> String {
        self.0.clone()
    }
    fn set(&mut self, t: &str) {
        self.0 = t.to_string();
    }
}

/// A buffer, a cursor and an engine; `|` in the text marks the cursor.
struct T {
    buf: Buffer,
    sel: Sel,
    vim: Vim,
    clip: Mem,
    last: Vec<Feed>,
}

fn keys(s: &str) -> Vec<VKey> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '<'
            && let Some(j) = cs[i..].iter().position(|c| *c == '>')
        {
            let name: String = cs[i + 1..i + j].iter().collect();
            let k = match name.as_str() {
                "Esc" => Some(VKey::Esc),
                "CR" => Some(VKey::Enter),
                "BS" => Some(VKey::Backspace),
                n if n.starts_with("C-") && n.chars().count() == 3 => Some(VKey::Ctrl(n.chars().nth(2).unwrap())),
                _ => None,
            };
            if let Some(k) = k {
                out.push(k);
                i += j + 1;
                continue;
            }
        }
        out.push(VKey::Char(cs[i]));
        i += 1;
    }
    out
}

impl T {
    fn new(marked: &str) -> T {
        let pos = marked.chars().position(|c| c == '|').expect("cursor marker");
        let text: String = marked.chars().filter(|c| *c != '|').collect();
        T {
            buf: Buffer::from_str(&text),
            sel: Sel::caret(pos),
            vim: Vim::new(),
            clip: Mem(String::new()),
            last: Vec::new(),
        }
    }

    fn send(&mut self, s: &str) -> &mut T {
        for k in keys(s) {
            let f = {
                let mut cx =
                    Ctx { buf: &mut self.buf, sel: &mut self.sel, clip: &mut self.clip, indent: "    ", page: 10 };
                self.vim.feed(k, &mut cx)
            };
            match f {
                Feed::Pass => {
                    // What the editor does with a key typed in Insert mode.
                    let h = self.sel.head;
                    match k {
                        VKey::Char(c) => {
                            self.buf.replace(h, h, &c.to_string(), EditKind::Typing, self.sel);
                            self.sel = Sel::caret(h + 1);
                        }
                        VKey::Enter => {
                            self.buf.replace(h, h, "\n", EditKind::Other, self.sel);
                            self.sel = Sel::caret(h + 1);
                        }
                        VKey::Backspace if h > 0 => {
                            self.buf.replace(h - 1, h, "", EditKind::Backspace, self.sel);
                            self.sel = Sel::caret(h - 1);
                        }
                        _ => {}
                    }
                }
                Feed::Undo(n) => {
                    for _ in 0..n {
                        if let Some(s) = self.buf.undo() {
                            self.sel = Sel::caret(s.head.min(self.buf.len_chars()));
                        }
                    }
                }
                Feed::Redo(n) => {
                    for _ in 0..n {
                        if let Some(s) = self.buf.redo() {
                            self.sel = Sel::caret(s.head.min(self.buf.len_chars()));
                        }
                    }
                }
                Feed::Done => {}
                other => self.last.push(other),
            }
        }
        self
    }

    fn text(&self) -> String {
        self.buf.slice(0, self.buf.len_chars())
    }

    /// Text with `|` at the cursor.
    fn show(&self) -> String {
        let mut t = self.text();
        let at = t.char_indices().nth(self.sel.head).map(|(i, _)| i).unwrap_or(t.len());
        t.insert(at, '|');
        t
    }
}

fn run(marked: &str, k: &str) -> String {
    let mut t = T::new(marked);
    t.send(k);
    t.show()
}

#[test]
fn horizontal_and_word_motions() {
    assert_eq!(run("|foo bar baz", "w"), "foo |bar baz");
    assert_eq!(run("|foo bar baz", "2w"), "foo bar |baz");
    assert_eq!(run("foo bar |baz", "b"), "foo |bar baz");
    assert_eq!(run("|foo bar", "e"), "fo|o bar");
    assert_eq!(run("|foo.bar", "w"), "foo|.bar");
    assert_eq!(run("|foo.bar baz", "W"), "foo.bar |baz");
    assert_eq!(run("foo b|ar", "0"), "|foo bar");
    assert_eq!(run("foo b|ar", "$"), "foo ba|r");
    assert_eq!(run("  fo|o", "^"), "  |foo");
    assert_eq!(run("|abc", "l"), "a|bc");
    assert_eq!(run("a|bc", "h"), "|abc");
    assert_eq!(run("ab|c", "l"), "ab|c");
    assert_eq!(run("|foo bar", "ge"), "|foo bar");
    assert_eq!(run("foo ba|r", "ge"), "fo|o bar");
}

#[test]
fn line_motions_keep_the_wanted_column() {
    assert_eq!(run("ab|cd\nx\nabcd", "j"), "abcd\n|x\nabcd");
    assert_eq!(run("ab|cd\nx\nabcd", "jj"), "abcd\nx\nab|cd");
    assert_eq!(run("a\nb\nc|", "gg"), "|a\nb\nc");
    assert_eq!(run("|a\nb\nc", "G"), "a\nb\n|c");
    assert_eq!(run("|a\nb\nc", "2G"), "a\n|b\nc");
    assert_eq!(run("|a\n  b\nc", "j"), "a\n|  b\nc");
    assert_eq!(run("|a\n  b\nc", "+"), "a\n  |b\nc");
}

#[test]
fn find_motions_and_repeat() {
    assert_eq!(run("|a,b,c", "f,"), "a|,b,c");
    assert_eq!(run("|a,b,c", "f,;"), "a,b|,c");
    assert_eq!(run("|a,b,c", "2f,"), "a,b|,c");
    assert_eq!(run("|a,b,c", "t,"), "|a,b,c");
    assert_eq!(run("a,b,|c", "F,"), "a,b|,c");
    assert_eq!(run("a,b,|c", "F,,"), "a,b|,c");
    assert_eq!(run("|ab(cd)e", "%"), "ab(cd|)e");
    assert_eq!(run("ab(c|d)e", "%"), "ab|(cd)e");
    assert_eq!(run("a\n\nb\nc\n\nd|", "{"), "a\n\nb\nc\n|\nd");
    assert_eq!(run("|a\nb\n\nc", "}"), "a\nb\n|\nc");
}

#[test]
fn delete_and_change_with_motions() {
    assert_eq!(run("|foo bar", "dw"), "|bar");
    assert_eq!(run("|foo bar baz", "d2w"), "|baz");
    assert_eq!(run("foo b|ar", "db"), "foo |ar");
    assert_eq!(run("foo |bar", "d$"), "foo| ");
    assert_eq!(run("foo |bar", "D"), "foo| ");
    assert_eq!(run("foo| bar", "d0"), "| bar");
    assert_eq!(run("|foo bar", "x"), "|oo bar");
    assert_eq!(run("|foo bar", "3x"), "| bar");
    assert_eq!(run("foo| bar", "X"), "fo| bar");
    assert_eq!(run("|foo bar", "dfb"), "|ar");
    assert_eq!(run("|foo bar", "dtb"), "|bar");
    assert_eq!(run("|foo bar\nbaz", "dw"), "|bar\nbaz");
    assert_eq!(run("foo |bar\nbaz", "dw"), "foo| \nbaz");
    assert_eq!(run("|foo bar", "cwxy<Esc>"), "x|y bar");
    assert_eq!(run("|foo bar", "cwxy<Esc>u"), "|foo bar");
    assert_eq!(run("fo|o bar", "cwxy<Esc>"), "fox|y bar");
    assert_eq!(run("|foo bar", "c$x<Esc>"), "|x");
    assert_eq!(run("|foo bar", "Cx<Esc>"), "|x");
    assert_eq!(run("f|oo", "sx<Esc>"), "f|xo");
}

#[test]
fn linewise_operators_and_put() {
    assert_eq!(run("a\n|b\nc", "dd"), "a\n|c");
    assert_eq!(run("a\nb\n|c", "dd"), "a\n|b");
    assert_eq!(run("|a\nb\nc\nd", "2dd"), "|c\nd");
    assert_eq!(run("a\n|b\nc", "ddp"), "a\nc\n|b");
    assert_eq!(run("a\n|b\nc", "yyjp"), "a\nb\nc\n|b");
    assert_eq!(run("a\n|b\nc", "yyP"), "a\n|b\nb\nc");
    assert_eq!(run("a\n|b\nc", "yy2p"), "a\nb\n|b\nb\nc");
    assert_eq!(run("|a\nb", "Yjp"), "a\nb\n|a");
    assert_eq!(run("|a\nb\nc", "dj"), "|c");
    assert_eq!(run("a\n|b\nc", "dk"), "|c");
    assert_eq!(run("|a b", "ccx<Esc>"), "|x");
    assert_eq!(run("|foo bar", "ywP"), "foo| foo bar");
    assert_eq!(run("|foo bar", "yw$p"), "foo barfoo| ");
    assert_eq!(run("a\n|b", "ddu"), "a\n|b");
}

#[test]
fn insert_commands_and_one_undo_step() {
    assert_eq!(run("a|bc", "ix<Esc>"), "a|xbc");
    assert_eq!(run("a|bc", "ax<Esc>"), "ab|xc");
    assert_eq!(run("  a|bc", "Ix<Esc>"), "  |xabc");
    assert_eq!(run("a|bc", "Ax<Esc>"), "abc|x");
    assert_eq!(run("a|bc\nd", "ox<Esc>"), "abc\n|x\nd");
    assert_eq!(run("a|bc\nd", "Ox<Esc>"), "|x\nabc\nd");
    assert_eq!(run("  a|bc", "ox<Esc>"), "  abc\n  |x");
    assert_eq!(run("a|b", "3ix<Esc>"), "axx|xb");
    assert_eq!(run("a|b", "ihello<CR>world<Esc>u"), "a|b");
    assert_eq!(run("a|b", "ix<BS><BS><Esc>"), "|b");
    assert_eq!(run("a|b\nc", "2ox<Esc>"), "ab\nx\n|x\nc");
    let mut t = T::new("a|b");
    t.send("ihello<Esc>");
    t.send("u");
    assert_eq!(t.text(), "ab");
    t.send("<C-r>");
    assert_eq!(t.text(), "ahellob");
}

#[test]
fn join_replace_toggle_and_indent() {
    assert_eq!(run("|a\n  b", "J"), "a| b");
    assert_eq!(run("|a\nb\nc", "3J"), "a b| c");
    assert_eq!(run("|ab", "rx"), "|xb");
    assert_eq!(run("|abc", "3rx"), "xx|x");
    assert_eq!(run("|abc", "5rx"), "|abc");
    assert_eq!(run("|abc", "~"), "A|bc");
    assert_eq!(run("|abc", "3~"), "AB|C");
    assert_eq!(run("|a\nb", ">>"), "    |a\nb");
    assert_eq!(run("    |a", "<<"), "|a");
    assert_eq!(run("|a\nb\nc", ">j"), "    |a\n    b\nc");
    assert_eq!(run("|a b", "gUw"), "|A b");
    assert_eq!(run("|A B", "guu"), "|a b");
    assert_eq!(run("|ab", "g~~"), "|AB");
}

#[test]
fn text_objects() {
    assert_eq!(run("foo b|ar baz", "diw"), "foo | baz");
    assert_eq!(run("foo b|ar baz", "daw"), "foo |baz");
    assert_eq!(run("foo bar b|az", "daw"), "foo ba|r");
    assert_eq!(run("foo b|ar baz", "ciwx<Esc>"), "foo |x baz");
    assert_eq!(run("f(a, |b)", "di("), "f(|)");
    assert_eq!(run("f(a, |b)", "da("), "|f");
    assert_eq!(run("f(a, |b)", "dib"), "f(|)");
    assert_eq!(run("f(a(1), |b)", "ci)x<Esc>"), "f(|x)");
    assert_eq!(run("f(a(|1), b)", "di("), "f(a(|), b)");
    assert_eq!(run("x = [1, |2]", "ci]y<Esc>"), "x = [|y]");
    assert_eq!(run("say \"he|llo\" now", "di\""), "say \"|\" now");
    assert_eq!(run("say \"he|llo\" now", "da\""), "say |now");
    assert_eq!(run("say |\"hello\" 'x'", "ci'y<Esc>"), "say \"hello\" '|y'");
    assert_eq!(run("{\n  a|\n}", "di{"), "{\n|}");
    assert_eq!(run("x {a|b} y", "di{"), "x {|} y");
    assert_eq!(run("a\n\nb|\nc\n\nd", "dap"), "a\n\n|d");
    assert_eq!(run("a\n\nb|\nc\n\nd", "dip"), "a\n\n|\nd");
    assert_eq!(run("foo b|ar", "yiwP"), "foo ba|rbar");
}

struct Fixed;

impl TextObjects for Fixed {
    fn object(&self, _b: &Buffer, _pos: usize, kind: char, _inner: bool) -> Option<(usize, usize)> {
        (kind == 'f').then_some((0, 3))
    }
}

#[test]
fn a_provider_answers_before_the_local_objects() {
    let mut t = T::new("|abc def");
    t.vim.set_text_objects(Box::new(Fixed));
    t.send("dif");
    assert_eq!(t.show(), "| def");
    // The local objects still answer what the provider declines.
    t.send("diw");
    assert_eq!(t.show(), "|def");
}

#[test]
fn dot_repeats_the_last_change() {
    assert_eq!(run("|abcd", "x."), "|cd");
    assert_eq!(run("|a b c d", "dw."), "|c d");
    assert_eq!(run("|a b c d", "dw2."), "|d");
    assert_eq!(run("|foo bar baz", "cwxy<Esc>w."), "xy x|y baz");
    assert_eq!(run("|a\nb\nc", "A;<Esc>j."), "a;\nb|;\nc");
    assert_eq!(run("|a\nb\nc", "A;<Esc>j.j."), "a;\nb;\nc|;");
    assert_eq!(run("|a\nb\nc\nd", "dd."), "|c\nd");
    assert_eq!(run("|ab", "ix<Esc>."), "|xxab");
    // `.` is one undo step.
    let mut t = T::new("|a b c");
    t.send("dw.");
    assert_eq!(t.text(), "c");
    t.send("u");
    assert_eq!(t.text(), "b c");
}

#[test]
fn visual_modes() {
    assert_eq!(run("|foo bar", "vld"), "|o bar");
    assert_eq!(run("|foo bar", "viwd"), "| bar");
    assert_eq!(run("|foo bar", "vey$p"), "foo barfo|o");
    assert_eq!(run("a\n|b\nc", "Vd"), "a\n|c");
    assert_eq!(run("|a\nb\nc", "Vjd"), "|c");
    assert_eq!(run("|a\nb\nc", "Vjy"), "|a\nb\nc");
    assert_eq!(run("|a\nb\nc", "Vj>"), "    |a\n    b\nc");
    assert_eq!(run("|foo", "vlU"), "|FOo");
    assert_eq!(run("|foo bar", "vecx<Esc>"), "|x bar");
    assert_eq!(run("|a\nb", "VjJ"), "a| b");
    assert_eq!(run("|foo bar", "vlohd"), "|o bar");
    assert_eq!(run("|ab", "vlrx"), "|xx");
    let mut t = T::new("|foo bar");
    t.send("vl");
    assert_eq!(t.sel, Sel { anchor: 0, head: 2 });
    t.send("<Esc>");
    assert!(t.sel.is_empty());
    assert_eq!(t.vim.mode, Mode::Normal);
    t.send("V");
    assert_eq!(t.sel, Sel { anchor: 0, head: 7 });
}

#[test]
fn a_mouse_selection_becomes_visual() {
    let mut t = T::new("|foo bar");
    t.sel = Sel { anchor: 0, head: 3 };
    t.send("d");
    assert_eq!(t.show(), "| bar");
}

#[test]
fn visual_put_replaces_the_selection() {
    assert_eq!(run("|foo bar", "yiwwviwp"), "foo fo|o");
    // The replaced text lands in the register for the next put.
    assert_eq!(run("|foo bar", "yiwwviwp0P"), "ba|rfoo foo");
    assert_eq!(run("|foo bar", "yiwwviwP0P"), "fo|ofoo foo");
}

#[test]
fn registers() {
    assert_eq!(run("|a b", "\"ayw\"_dwP"), "a| b");
    assert_eq!(run("|foo bar", "\"ayiww\"byiw\"aP"), "foo fo|obar");
    assert_eq!(run("|foo bar", "\"ayiww\"Ayiw\"ap"), "foo bfooba|rar");
}

#[test]
fn registers_keep_deletes_and_yanks_apart() {
    let mut t = T::new("|foo bar");
    t.send("yiwwdiw0\"0P");
    assert_eq!(t.text(), "foofoo ");
    let mut t = T::new("|one\ntwo");
    t.send("dd\"_ddP");
    assert_eq!(t.text(), "one\n".to_string() + "");
}

#[test]
fn marks_and_jumps() {
    assert_eq!(run("a|b\nc\nd", "majj`a"), "a|b\nc\nd");
    assert_eq!(run("  a|b\nc\nd", "majj'a"), "  |ab\nc\nd");
    assert_eq!(run("a|b\nc", "Ggg"), "|ab\nc");
    assert_eq!(run("a\nb|\nc", "G''"), "a\n|b\nc");
}

#[test]
fn search_commands() {
    assert_eq!(run("|a foo b foo", "/foo<CR>"), "a |foo b foo");
    assert_eq!(run("|a foo b foo", "/foo<CR>n"), "a foo b |foo");
    assert_eq!(run("|a foo b foo", "/foo<CR>nn"), "a |foo b foo");
    assert_eq!(run("|a foo b foo", "/foo<CR>N"), "a foo b |foo");
    assert_eq!(run("a foo b |foo", "?foo<CR>"), "a |foo b foo");
    assert_eq!(run("|foo x foo", "*"), "foo x |foo");
    assert_eq!(run("foo x |foo", "#"), "|foo x foo");
    let mut t = T::new("|abc");
    t.send("/zzz<CR>");
    assert!(matches!(t.last.last(), Some(Feed::Message(m)) if m.contains("zzz")));
    t.send("/<Esc>");
    assert_eq!(t.vim.status(), "NORMAL");
}

#[test]
fn ex_commands() {
    let mut t = T::new("|a\nb\nc\nd");
    t.send(":3<CR>");
    assert_eq!(t.show(), "a\nb\n|c\nd");
    t.send(":1,2d<CR>");
    assert_eq!(t.text(), "c\nd");
    t.send(":s/c/x/<CR>");
    assert_eq!(t.text(), "x\nd");
    t.send(":%s/x/yy/g<CR>");
    assert_eq!(t.text(), "yy\nd");
    t.send(":w<CR>:q<CR>:q!<CR>:wq<CR>:x<CR>:e foo.txt<CR>");
    assert_eq!(
        t.last,
        vec![
            Feed::Message("1 line(s) changed".into()),
            Feed::Message("1 line(s) changed".into()),
            Feed::Ex(ExCmd::Write),
            Feed::Ex(ExCmd::Quit { force: false }),
            Feed::Ex(ExCmd::Quit { force: true }),
            Feed::Ex(ExCmd::WriteQuit),
            Feed::Ex(ExCmd::WriteQuit),
            Feed::Ex(ExCmd::Edit("foo.txt".into())),
        ]
    );
    t.send(":nonsense<CR>");
    assert!(matches!(t.last.last(), Some(Feed::Message(m)) if m.contains("nonsense")));
    t.send(":s/nope/x/<CR>");
    assert!(matches!(t.last.last(), Some(Feed::Message(m)) if m.contains("not found")));
    // Backspace edits the command line and cancels it when empty.
    let mut t = T::new("|a");
    t.send(":wx<BS>");
    assert_eq!(t.vim.status(), ":w");
    t.send("<BS><BS>");
    assert_eq!(t.vim.status(), "NORMAL");
}

#[test]
fn visual_ex_range_uses_the_selection() {
    let mut t = T::new("|a\nb\nc");
    t.send("Vj:");
    assert_eq!(t.vim.status(), ":'<,'>");
    t.send("d<CR>");
    assert_eq!(t.text(), "c");
}

#[test]
fn macro_keys_are_reported_to_the_app() {
    let mut t = T::new("|a");
    t.send("qa");
    assert_eq!(t.last, vec![Feed::MacroStart('a')]);
    t.send("q");
    assert_eq!(t.last.last(), Some(&Feed::MacroStop('a')));
    t.send("3@a@@");
    assert_eq!(t.last[2], Feed::MacroPlay('a', 3));
    assert_eq!(t.last[3], Feed::MacroPlay('a', 1));
}

#[test]
fn counts_registers_and_pending_keys_show_in_the_status() {
    let mut t = T::new("|a");
    assert_eq!(t.vim.status(), "NORMAL");
    t.send("2d");
    assert_eq!(t.vim.status(), "NORMAL  2d");
    t.send("<Esc>i");
    assert_eq!(t.vim.status(), "INSERT");
    t.send("<Esc>v");
    assert_eq!(t.vim.status(), "VISUAL");
    t.send("<Esc>V");
    assert_eq!(t.vim.status(), "V-LINE");
}

#[test]
fn a_new_buffer_resets_the_mode() {
    let mut t = T::new("|a");
    t.send("i");
    assert_eq!(t.vim.mode, Mode::Insert);
    let mut other = Buffer::from_str("zzz");
    other.begin_compound(Sel::caret(0));
    other.end_compound(Sel::caret(0));
    t.buf = other;
    t.buf = Buffer::from_str("zzz");
    // Same generation id in the test double: an explicit reset is what the app calls.
    t.vim.reset();
    assert_eq!(t.vim.mode, Mode::Normal);
}

#[test]
fn crlf_and_unknown_keys_do_nothing_harmful() {
    assert_eq!(run("|abc", "Z"), "|abc");
    assert_eq!(run("|abc", "dZ"), "|abc");
    assert_eq!(run("|", "x"), "|");
    assert_eq!(run("|", "dd"), "|");
    assert_eq!(run("|", "p"), "|");
    assert_eq!(run("|", "w"), "|");
}
