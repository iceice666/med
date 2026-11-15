use crate::*;
use med_core::Buffer;

struct Harness {
    buf: Buffer,
    vim: Vim,
    shared: Shared,
    clipboard: MemClipboard,
    lang: Lang,
    effects: Vec<Effect>,
}

/// Parse `"dw<esc>"`-style key notation.
fn parse_keys(keys: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut rest = keys;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let name = rest[1..end].to_ascii_lowercase();
            let key = match name.as_str() {
                "esc" => Some(Key::Esc),
                "cr" => Some(Key::Enter),
                "bs" => Some(Key::Backspace),
                "del" => Some(Key::Delete),
                "tab" => Some(Key::Tab),
                "lt" => Some(Key::Char('<')),
                "up" => Some(Key::Up),
                "down" => Some(Key::Down),
                "left" => Some(Key::Left),
                "right" => Some(Key::Right),
                n if n.starts_with("c-") && n.len() == 3 => {
                    Some(Key::Ctrl(n.chars().nth(2).unwrap()))
                }
                _ => None,
            };
            if let Some(key) = key {
                out.push(key);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push(Key::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    out
}

impl Harness {
    /// `|` in `text` marks the cursor.
    fn new(text: &str) -> Self {
        let cursor = text.chars().position(|c| c == '|').unwrap_or(0);
        let buf = Buffer::new(&text.replacen('|', "", 1));
        let mut vim = Vim::new();
        vim.cursor = cursor;
        Self {
            buf,
            vim,
            shared: Shared::default(),
            clipboard: MemClipboard::default(),
            lang: Lang {
                line_comment: Some("//".into()),
                ..Lang::default()
            },
            effects: Vec::new(),
        }
    }

    fn keys(&mut self, keys: &str) -> &mut Self {
        for key in parse_keys(keys) {
            let mut cx = Ctx {
                buf: &mut self.buf,
                shared: &mut self.shared,
                clipboard: &mut self.clipboard,
                viewport: Viewport { top: 0, height: 10 },
                lang: &self.lang,
            };
            let fx = self.vim.handle_key(key, &mut cx);
            self.effects.extend(fx);
        }
        self
    }

    /// Buffer text with `|` at the cursor, minus the final newline.
    fn state(&self) -> String {
        let mut text: Vec<char> = self.buf.text().chars().collect();
        text.insert(self.vim.cursor, '|');
        text.pop();
        text.into_iter().collect()
    }
}

#[track_caller]
fn check(before: &str, keys: &str, after: &str) {
    let mut h = Harness::new(before);
    h.keys(keys);
    assert_eq!(h.state(), after, "keys: {keys}");
}

#[test]
fn basic_motions() {
    check("|hello world", "w", "hello |world");
    check("|hello world", "e", "hell|o world");
    check("hello wor|ld", "b", "hello |world");
    check("hello |world", "ge", "hell|o world");
    check("|hello world", "$", "hello worl|d");
    check("hello wor|ld", "0", "|hello world");
    check("  hello wor|ld", "^", "  |hello world");
    check("|a\nb\nc", "G", "a\nb\n|c");
    check("a\nb\n|c", "gg", "|a\nb\nc");
    check("|a\nb\nc", "2G", "a\n|b\nc");
    check("|a.b c", "W", "a.b |c");
    check("|a.b c", "w", "a|.b c");
    check("|ab\n\ncd", "w", "ab\n|\ncd");
    check("ab\n\n|cd", "b", "ab\n|\ncd");
    check("|ab", "l", "a|b");
    check("a|b", "l", "a|b");
    check("|你好 世界", "w", "你好 |世界");
}

#[test]
fn vertical_keeps_goal_column() {
    check("abc|d\nx\nabcdef", "jj", "abcd\nx\nabc|def");
    check("abcd|e\nx\nabcdef", "$jj", "abcde\nx\nabcde|f");
    check("|a\nb", "k", "|a\nb");
}

#[test]
fn find_and_repeat() {
    check("|a,b,c", "f,", "a|,b,c");
    check("|a,b,c", "f,;", "a,b|,c");
    check("|a,b,c", "f,;,", "a|,b,c");
    check("|a,b,c", "t,;", "a,|b,c");
    check("a,b,|c", "F,", "a,b|,c");
    check("|a,b,c", "dt,", "|,b,c");
    check("|a,b,c", "df,", "|b,c");
    check("|a,b,c", "2f,", "a,b|,c");
}

#[test]
fn match_pair_and_paragraphs() {
    check("|(a(b))", "%", "(a(b)|)");
    check("(a(b)|)", "%", "|(a(b))");
    check("x |= f(a)", "%", "x = f(a|)");
    check("|a\nb\n\nc", "}", "a\nb\n|\nc");
    check("a\nb\n\n|c", "{", "a\nb\n|\nc");
}

#[test]
fn delete() {
    check("|hello world", "dw", "|world");
    check("hello |world", "x", "hello |orld");
    check("|a b c d", "2dw", "|c d");
    check("|a b c d", "d2w", "|c d");
    check("x|yz", "D", "|x");
    check("abc |def", "d0", "|def");
    check("abc |def", "d$", "abc| ");
    check("ab|c", "X", "a|c");
    check("|abcd", "3x", "|d");
    check("foo |bar\nbaz", "dw", "foo| \nbaz");
    check("foo |bar", "de", "foo| ");
    check("|a\nb\nc", "dj", "|c");
    check("a\n|b\nc", "dk", "|c");
    check("a\n|b\nc", "dG", "|a");
    check("a\n|b\nc", "dgg", "|c");
}

#[test]
fn linewise() {
    check("|one\ntwo\nthree", "dd", "|two\nthree");
    check("one\ntwo\n|three", "dd", "one\n|two");
    check("|one", "dd", "|");
    check("|one\ntwo\nthree", "2dd", "|three");
    check("on|e\ntwo", "ddp", "two\n|one");
    check("on|e\ntwo", "yyp", "one\n|one\ntwo");
    check("on|e\ntwo", "yyP", "|one\none\ntwo");
    check("on|e\ntwo", "Yjp", "one\ntwo\n|one");
    check("  on|e\ntwo", "ccx<esc>", "  |x\ntwo");
    check("  on|e\ntwo", "Sx<esc>", "  |x\ntwo");
}

#[test]
fn change() {
    check("|foo bar", "cwbaz<esc>", "ba|z bar");
    check("fo|o bar", "cwx<esc>", "fo|x bar");
    check("foo| bar", "cwx<esc>", "foo|xbar");
    check("|foo bar", "c2wx<esc>", "|x");
    check("foo |bar baz", "Cx<esc>", "foo |x");
    check("|abc", "sx<esc>", "|xbc");
    check("|foo.bar", "ciwx<esc>", "|x.bar");
}

#[test]
fn text_objects() {
    check("foo(a, |b)", "ci(x<esc>", "foo(|x)");
    check("foo(a, |b)", "dib", "foo(|)");
    check("foo(a, |b) c", "da(", "foo| c");
    check("say \"hel|lo\" now", "di\"", "say \"|\" now");
    check("say \"hel|lo\" now", "da\"", "say |now");
    check("|say \"hello\" now", "ci\"x<esc>", "say \"|x\" now");
    check("a |foo b", "diw", "a | b");
    check("a |foo b", "daw", "a |b");
    check("a fo|o", "daw", "|a");
    check("[1, [2|, 3]]", "di[", "[1, [|]]");
    check("a <b|c> d", "di<lt>", "a <|> d");
    check("fn x() {\n    a\n    |b\n}", "di{", "fn x() {\n|}");
    check(
        "fn x() {\n    a\n    |b\n}",
        "ci{y<esc>",
        "fn x() {\n    |y\n}",
    );
    check("fn x() {\n|}", "di{", "fn x() {\n|}");
    check("a\n|b\nc\n\nd", "dip", "|\nd");
    check("a\n|b\nc\n\nd", "dap", "|d");
    check("x\n\na\n|b", "dap", "|x");
}

#[test]
fn yank_and_put() {
    check("|abc def", "yiwwP", "abc ab|cdef");
    check("|abc def", "yiwwp", "abc dab|cef");
    check("|ab", "xp", "b|a");
    check("|a b", "ylvp", "|a b");
    check("|a\nb", "yyjVp", "a\n|a");
    check("|ab cd", "yiwwviwp", "ab |ab");
    check("|ab", "\"ayl\"bx\"aP\"bP", "|aab");
    check("|ab", "yl3p", "aaa|ab");
}

#[test]
fn put_from_system_clipboard() {
    let mut h = Harness::new("|ab");
    h.clipboard.0 = Some("XY".into());
    h.keys("p");
    assert_eq!(h.state(), "aX|Yb");

    let mut h = Harness::new("|ab");
    h.clipboard.0 = Some("line\n".into());
    h.keys("p");
    assert_eq!(h.state(), "ab\n|line");

    let mut h = Harness::new("|ab cd");
    h.keys("yiw");
    assert_eq!(h.clipboard.0.as_deref(), Some("ab"));
    h.keys("\"_dw");
    assert_eq!(h.clipboard.0.as_deref(), Some("ab"));
}

#[test]
fn undo_redo() {
    check("|abc", "xu", "|abc");
    check("|abc", "xxu", "|bc");
    check("|abc", "xxuu", "|abc");
    check("|abc", "xxuu<c-r>", "|bc");
    check("|abc", "ixy<esc>u", "|abc");
    check("|abc", "ixy<esc>u<c-r>", "|xyabc");
    check("a|bc", "cwxyz<esc>u", "a|bc");
    check("|a\nb\nc", "ddddu", "|b\nc");
    check("|a\nb", ">>u", "|a\nb");
    check("|a b c", "dw.uu", "|a b c");
}

#[test]
fn dot_repeat() {
    check("|foo bar", "cwbaz<esc>w.", "baz ba|z");
    check("|a\nb\nc", "dd.", "|c");
    check("|a b c", "dw.", "|c");
    check("|abc", "x.", "|c");
    check("|a\nb", "A!<esc>j.", "a!\nb|!");
    check("|a\nb", "ox<esc>.", "a\nx\n|x\nb");
    check("|a\nb", ">>j.", "    a\n    |b");
    // Motions in between must not clobber the repeatable change.
    check("|a b c d", "dwwl0.", "|c d");
}

#[test]
fn insert_mode() {
    check("|abc", "ix<esc>", "|xabc");
    check("|abc", "ax<esc>", "a|xbc");
    check("a|bc", "Ix<esc>", "|xabc");
    check("|abc", "Ax<esc>", "abc|x");
    check("|a", "ob<esc>", "a\n|b");
    check("|a", "Ob<esc>", "|b\na");
    check("  |a", "ob<esc>", "  a\n  |b");
    check("|", "i<tab>x<bs><bs><esc>", "|");
    check("|", "ia<cr>b<esc>", "a\n|b");
    check("ab|c", "i<bs><bs><bs><esc>", "|c");
    check("a\n|b", "i<bs><esc>", "|ab");
    check("|foo bar", "A<c-w><esc>", "foo| ");
    check("|ab", "i<right>x<esc>", "a|xb");
}

#[test]
fn auto_indent() {
    check("|if x {", "A<cr>y<esc>", "if x {\n    |y");
    check("|if x {}", "$i<cr>y<esc>", "if x {\n    |y\n}");
    check("|if x {", "oy<esc>", "if x {\n    |y");
    check("    |a", "A<cr><cr>b<esc>", "    a\n\n    |b");
    // Unused auto-indent is removed on escape.
    check("    |a", "o<esc>", "    a\n|");
    check("|if x {", "oa<cr>}<esc>", "if x {\n    a\n|}");
}

#[test]
fn visual() {
    check("|abc def", "vld", "|c def");
    check("|abc def", "ved", "| def");
    check("|a\nb\nc", "Vjd", "|c");
    check("|abc def", "viwc!<esc>", "|! def");
    check("abc |def", "vbd", "|ef");
    check("|abc", "vly", "|abc");
    check("|abc", "vlU", "|ABc");
    check("|a\nb\nc", "VjJ", "a| b\nc");
    check("|abc", "vlrx", "|xxc");
    check("|a\nb", "Vj>", "    |a\n    b");
    check("abc |def ghi", "vo", "abc |def ghi");
    check("|abc", "v<esc>x", "|bc");
    check("|a\nb\nc", "jVkd", "|c");
    check("|ab cd", "vey$vp", "ab c|ab");
}

#[test]
fn case_and_misc() {
    check("|abc d", "gUiw", "|ABC d");
    check("|ABC d", "guiw", "|abc d");
    check("|aBc", "g~~", "|AbC");
    check("|abc", "~~", "AB|c");
    check("|a\n  b", "J", "a| b");
    check("|a\n  b", "gJ", "a|  b");
    check("|a\nb\nc", "3J", "a b| c");
    check("|abc", "rx", "|xbc");
    check("|abc", "3rx", "xx|x");
    check("|abc", "4rx", "|abc");
    check("|a", ">>", "    |a");
    check("      |a", "<<", "  |a");
    check("|a\nb", ">j", "    |a\n    b");
}

#[test]
fn comments() {
    check("|a\nb", "gcc", "|// a\nb");
    check("|// a\nb", "gcc", "|a\nb");
    check("|a\n  b", "gcj", "|// a\n//   b");
    check("  |a\n  b", "Vjgc", "  |// a\n  // b");
    check("  |// a\n  // b", "gcj", "  |a\n  b");
}

#[test]
fn search() {
    check("|a\nfoo\nb\nfoo", "/foo<cr>", "a\n|foo\nb\nfoo");
    check("|a\nfoo\nb\nfoo", "/foo<cr>n", "a\nfoo\nb\n|foo");
    check("|a\nfoo\nb\nfoo", "/foo<cr>nn", "a\n|foo\nb\nfoo");
    check("|a\nfoo\nb\nfoo", "/foo<cr>N", "a\nfoo\nb\n|foo");
    check("a\nfoo\nb\n|foo", "?foo<cr>", "a\n|foo\nb\nfoo");
    check("|foo x foo", "*", "foo x |foo");
    check("foo x |foo", "#", "|foo x foo");
    check("|foo foobar foo", "*", "foo foobar |foo");
    check("|a b", "/zzz<cr>", "|a b");
    check("|x Foo foo", "/foo<cr>", "x |Foo foo");
    check("|x Foo foo", "/Foo<cr>n", "x |Foo foo");
}

#[test]
fn ex_commands() {
    check("|foo foo foo", ":s/foo/bar/g<cr>", "|bar bar bar");
    check("|foo foo foo", ":s/foo/bar<cr>", "|bar foo foo");
    check("|a\na\na", ":%s/a/b/<cr>", "b\nb\n|b");
    check("|a\na\na", ":2,3s/a/b/<cr>", "a\nb\n|b");
    check("|a\na\na", "Vj:s/a/b/<cr>", "b\n|b\na");
    check("|ab", ":s/\\(a\\)\\(b\\)/x/<cr>", "|ab");
    check("|ab", ":s/(a)(b)/\\2\\1/<cr>", "|ba");
    check("|a\nb\nc", ":3<cr>", "a\nb\n|c");
    check("|a\nb\nc", ":$<cr>", "a\nb\n|c");
    check("|a\nb\nc", ":2,3d<cr>", "|a");
    check("|a\nb\nc", ":x<bs><bs>", "|a\nb\nc");

    let mut h = Harness::new("|a");
    h.keys(":w foo.txt<cr>");
    assert_eq!(h.effects, vec![Effect::Ex("w foo.txt".into())]);
    assert_eq!(h.vim.mode, Mode::Normal);
}

#[test]
fn effects() {
    let mut h = Harness::new("|a");
    h.keys("gdKzz]d[d");
    assert_eq!(
        h.effects,
        vec![
            Effect::GotoDefinition,
            Effect::Hover,
            Effect::Scroll(Scroll::Center),
            Effect::NextDiagnostic,
            Effect::PrevDiagnostic,
        ]
    );

    let text = (0..50)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = Harness::new(&format!("|{text}"));
    h.keys("<c-d>");
    assert_eq!(h.buf.line_of(h.vim.cursor), 5);
    assert_eq!(h.effects, vec![Effect::Scroll(Scroll::Lines(5))]);
    h.keys("L");
    assert_eq!(h.buf.line_of(h.vim.cursor), 9);
}

#[test]
fn typed_text_and_paste() {
    let mut h = Harness::new("|if x {");
    h.keys("A");
    let mut cx = Ctx {
        buf: &mut h.buf,
        shared: &mut h.shared,
        clipboard: &mut h.clipboard,
        viewport: Viewport { top: 0, height: 10 },
        lang: &h.lang,
    };
    h.vim.type_text("\n好", &mut cx);
    h.vim.paste("\n  }\n", &mut cx);
    h.keys("<esc>");
    assert_eq!(h.state(), "if x {\n    好\n  }\n|");
    h.keys("u");
    assert_eq!(h.state(), "|if x {");
}

#[test]
fn pending_display_and_selection() {
    let mut h = Harness::new("|abc def");
    h.keys("\"a2d");
    assert_eq!(h.vim.pending_display(), "\"a2d");
    assert!(h.vim.has_pending());
    h.keys("<esc>vl");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 2)));
    h.keys("V");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 8)));
}
