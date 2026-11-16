use crate::*;
use med_core::Buffer;

struct Harness {
    buf: Buffer,
    vim: Vim,
    shared: Shared,
    clipboard: MemClipboard,
    lang: Lang,
    effects: Vec<Effect>,
    syntax: Option<Box<dyn SyntaxTree>>,
    viewport: Viewport,
}

fn syntax_of(syntax: &mut Option<Box<dyn SyntaxTree>>) -> Option<&mut dyn SyntaxTree> {
    match syntax {
        Some(syntax) => Some(&mut **syntax),
        None => None,
    }
}

impl Harness {
    /// `|` in `text` marks the cursor.
    fn new(text: &str) -> Self {
        let cursor = text.chars().position(|c| c == '|').unwrap_or(0);
        let buf = Buffer::new(&text.replacen('|', "", 1));
        let mut vim = Vim::new();
        vim.cursor = cursor;
        let mut shared = Shared::default();
        shared.keymap = Keymap::vim();
        Self {
            buf,
            vim,
            shared,
            clipboard: MemClipboard::default(),
            lang: Lang {
                line_comment: Some("//".into()),
                ..Lang::default()
            },
            effects: Vec::new(),
            syntax: None,
            viewport: Viewport {
                top: 0,
                height: 10,
                scroll_off: 0,
            },
        }
    }

    fn keys(&mut self, keys: &str) -> &mut Self {
        for key in parse_keys(keys) {
            let mut cx = Ctx {
                buf: &mut self.buf,
                shared: &mut self.shared,
                clipboard: &mut self.clipboard,
                viewport: self.viewport,
                lang: &self.lang,
                syntax: syntax_of(&mut self.syntax),
            };
            let fx = self.vim.handle_key(key, &mut cx);
            self.effects.extend(fx);
        }
        self
    }

    fn timeout(&mut self) {
        let mut cx = Ctx {
            buf: &mut self.buf,
            shared: &mut self.shared,
            clipboard: &mut self.clipboard,
            viewport: self.viewport,
            lang: &self.lang,
            syntax: syntax_of(&mut self.syntax),
        };
        let fx = self.vim.timeout(&mut cx);
        self.effects.extend(fx);
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
fn screen_motions_keep_the_scroll_margin() {
    let text: String = (0..30).map(|n| format!("{n}\n")).collect();
    let line = |h: &Harness| h.buf.line_of(h.vim.cursor);
    let at = |top: usize, keys: &str| {
        let mut h = Harness::new(&text);
        h.viewport = Viewport {
            top,
            height: 10,
            scroll_off: 2,
        };
        h.vim.cursor = h.buf.line_start(top + 4);
        h.keys(keys);
        line(&h)
    };
    // Lines 10 to 19 on screen: `H` and `L` stop two lines in.
    assert_eq!(at(10, "H"), 12);
    assert_eq!(at(10, "L"), 17);
    // A count further in than that goes there.
    assert_eq!(at(10, "4H"), 13);
    assert_eq!(at(10, "4L"), 16);
    // An edge that is the file's own: the view can go no further.
    assert_eq!(at(0, "H"), 0);
    assert_eq!(at(20, "L"), 29);
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
    // As vim's with 'ignorecase': whatever the case of the word.
    check("|Foo x FOO", "*", "Foo x |FOO");
    check("foo x |Foo", "#", "|foo x Foo");
    check("|Foo x foo x FOO", "**", "Foo x foo x |FOO");
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
fn the_engines_ex_names_are_its_own() {
    for name in EX_NAMES {
        let mut h = Harness::new("|a\nb");
        h.keys(&format!(":{name}<cr>"));
        assert_eq!(h.effects, vec![], ":{name} was left to the host");
    }
}

#[test]
fn the_host_replaces_the_command_line() {
    let mut h = Harness::new("|a");
    h.vim.set_cmdline_text("ignored");
    assert_eq!(h.vim.cmdline(), None);
    h.keys(":wr");
    h.vim.set_cmdline_text("write");
    assert_eq!(h.vim.cmdline(), Some((':', "write")));
    h.keys("<cr>");
    assert_eq!(h.effects, vec![Effect::Ex("write".into())]);
}

#[test]
fn the_command_line_is_edited_at_its_cursor() {
    let mut h = Harness::new("|a");
    h.keys(":wrte<left><left>i");
    assert_eq!(h.vim.cmdline(), Some((':', "write")));
    assert_eq!(h.vim.cmdline_cursor(), Some(3));
    h.keys("<home>x<del><end><bs>e<left><right>!");
    assert_eq!(h.vim.cmdline(), Some((':', "xrite!")));
    h.keys("<c-b><right><right><c-u>");
    assert_eq!(h.vim.cmdline(), Some((':', "ite!")));
    h.keys("<c-e> a b<left><left><c-w>");
    assert_eq!(h.vim.cmdline(), Some((':', "ite!  b")));
    assert_eq!(h.vim.cmdline_cursor(), Some(5));
    // Past either end, nothing to move over; wide chars move as one.
    h.keys("<c-u><left>é<left><left><del>");
    assert_eq!(h.vim.cmdline(), Some((':', " b")));
    h.keys("<esc>");

    // Completed from outside: the cursor goes to its end.
    h.keys(":wr<left>");
    h.vim.set_cmdline_text("write");
    assert_eq!(h.vim.cmdline_cursor(), Some(5));
}

#[test]
fn the_command_line_recalls_its_history() {
    let mut h = Harness::new("|a\na\na");
    h.keys(":foo<cr>:bar 1<cr>:foo 2<cr>/a<cr>");
    let ex = |e: &Effect| matches!(e, Effect::Ex(_));
    assert_eq!(h.effects.iter().filter(|e| ex(e)).count(), 3);

    // Back through them all, newest first, then forward to what was typed.
    h.keys(":x<c-p>");
    assert_eq!(h.vim.cmdline(), Some((':', "foo 2")));
    h.keys("<c-p><c-p><c-p>");
    assert_eq!(h.vim.cmdline(), Some((':', "foo")), "stops at the oldest");
    h.keys("<down><down><down>");
    assert_eq!(h.vim.cmdline(), Some((':', "x")));
    h.keys("<esc>");

    // `Up` keeps to the lines that start as the typed one does.
    h.keys(":fo<up>");
    assert_eq!(h.vim.cmdline(), Some((':', "foo 2")));
    assert_eq!(h.vim.cmdline_cursor(), Some(5));
    h.keys("<up>");
    assert_eq!(h.vim.cmdline(), Some((':', "foo")));
    // An edit ends the walk: the next `Up` looks for the new line.
    h.keys("<bs><bs><bs>b<up>");
    assert_eq!(h.vim.cmdline(), Some((':', "bar 1")));
    h.keys("<esc>");

    // Run again, a line moves to the newest.
    h.keys(":foo<cr>:<up>");
    assert_eq!(h.vim.cmdline(), Some((':', "foo")));
    h.keys("<esc>");

    // Searches have their own.
    h.keys("?<up>");
    assert_eq!(h.vim.cmdline(), Some(('?', "a")));
}

#[test]
fn effects() {
    let mut h = Harness::new("|a");
    h.keys("zzzt<c-e>");
    assert_eq!(
        h.effects,
        vec![
            Effect::Scroll(Scroll::Center),
            Effect::Scroll(Scroll::Top),
            Effect::Scroll(Scroll::Lines(1)),
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
        viewport: Viewport {
            top: 0,
            height: 10,
            scroll_off: 0,
        },
        lang: &h.lang,
        syntax: None,
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

#[test]
fn says_what_a_half_typed_command_waits_for() {
    let mut h = Harness::new("|abc def");
    let pending = |h: &Harness| {
        let pending = h.vim.pending_keys()?;
        Some((pending.mode, pending.keys, pending.typed, pending.awaiting))
    };
    let text = String::from;
    assert_eq!(pending(&h), None);

    // The start of a binding: what follows is looked up after it.
    h.keys("g");
    assert_eq!(
        pending(&h),
        Some((MapMode::Normal, text("g"), text("g"), None))
    );
    h.keys("<esc>");
    assert_eq!(pending(&h), None);

    // After a count and an operator, any motion: no keys of one yet.
    h.keys("2d");
    assert_eq!(
        pending(&h),
        Some((MapMode::Operator, text(""), text("2d"), None))
    );
    h.keys("g");
    assert_eq!(
        pending(&h),
        Some((MapMode::Operator, text("g"), text("2dg"), None))
    );
    h.keys("<esc>");

    // Keys taken as they are, not looked up.
    for (keys, mode, awaiting) in [
        ("\"", MapMode::Normal, Awaiting::Register),
        ("f", MapMode::Normal, Awaiting::Char),
        ("r", MapMode::Normal, Awaiting::Char),
        ("di", MapMode::Operator, Awaiting::TextObject),
    ] {
        h.keys(keys);
        assert_eq!(
            pending(&h),
            Some((mode, text(""), text(keys), Some(awaiting))),
            "{keys}"
        );
        h.keys("<esc>");
        assert_eq!(pending(&h), None, "{keys}");
    }

    // A visual selection has a table of its own.
    h.keys("vg");
    assert_eq!(pending(&h).map(|pending| pending.0), Some(MapMode::Visual));
    h.keys("<esc><esc>");

    // In insert mode, a key held back as the start of a binding.
    h.shared.keymap.bind(MapMode::Insert, "jk", "insert.escape");
    h.keys("ij");
    assert_eq!(
        pending(&h),
        Some((MapMode::Insert, text("j"), text("j"), None))
    );
    h.keys("k");
    assert_eq!(pending(&h), None);
}

fn command(name: &str, count: Option<usize>) -> Effect {
    Effect::Command {
        name: name.to_string(),
        count,
    }
}

#[test]
fn host_commands() {
    let mut h = Harness::new("|abc");
    let keymap = &mut h.shared.keymap;
    keymap.bind(MapMode::Normal, "gd", "editor.goto_definition");
    keymap.bind(MapMode::Normal, "]d", "editor.next_diagnostic");
    keymap.bind(MapMode::Visual, "K", "editor.hover");
    keymap.bind(MapMode::Insert, "<C-n>", "editor.complete");
    assert_eq!(
        keymap.command(MapMode::Normal, "gd"),
        Some("editor.goto_definition")
    );

    h.keys("gd3]dvK<esc>i<c-n>x<esc>");
    assert_eq!(
        h.effects,
        vec![
            command("editor.goto_definition", None),
            command("editor.next_diagnostic", Some(3)),
            command("editor.hover", None),
            command("editor.complete", None),
        ]
    );
    assert_eq!(h.state(), "|xabc");
    // The rest of `g` still works, and an unbound `g` combination is dropped.
    h.keys("$gg");
    assert_eq!(h.state(), "|xabc");
    h.keys("gqx");
    assert_eq!(h.state(), "|abc");
}

#[test]
fn a_buffers_own_keys() {
    let mut h = Harness::new("a |b c\nd e");
    let keymap = &mut h.shared.keymap;
    keymap.bind_in(Some("list"), MapMode::Normal, "x", "motion.line_end");
    keymap.bind_in(Some("list"), MapMode::Normal, "gf", "motion.down");
    keymap.bind_in(Some("list"), MapMode::Insert, "<CR>", "insert.escape");
    assert!(keymap.has_table("list"));

    // A buffer in no table has the mode's keys only.
    h.keys("x");
    assert_eq!(h.state(), "a | c\nd e");
    h.keys("u");

    // In one, its keys come first; what it does not know is the mode's.
    h.vim.tables = vec!["list".into()];
    h.keys("x");
    assert_eq!(h.state(), "a b |c\nd e");
    h.keys("gf");
    assert_eq!(h.state(), "a b c\nd |e");
    h.keys("gg");
    assert_eq!(h.state(), "|a b c\nd e");
    h.keys("dw");
    assert_eq!(h.state(), "|b c\nd e");
    h.keys("i<CR>");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(h.state(), "|b c\nd e");

    // An earlier table has the say over a later one.
    let keymap = &mut h.shared.keymap;
    keymap.bind_in(Some("deep"), MapMode::Normal, "x", "edit.delete_char");
    h.vim.tables = vec!["deep".into(), "list".into()];
    h.keys("x");
    assert_eq!(h.state(), "| c\nd e");

    // A half-typed binding says which tables it is looked up in.
    h.keys("g");
    let pending = h.vim.pending_keys().unwrap();
    assert_eq!(pending.tables, ["deep", "list"]);
    h.keys("<esc>");

    assert!(
        h.shared
            .keymap
            .unbind_in(Some("list"), MapMode::Normal, "x")
    );
    assert!(
        !h.shared
            .keymap
            .unbind_in(Some("nope"), MapMode::Normal, "x")
    );
    let next = h
        .shared
        .keymap
        .next_keys_in(Some("list"), MapMode::Normal, "g");
    assert_eq!(next.len(), 1);
}

#[test]
fn rebinding() {
    // Any key can be an operator; doubling it still means "this line".
    let mut h = Harness::new("a |b c\nd");
    for mode in [MapMode::Normal, MapMode::Operator] {
        h.shared.keymap.bind(mode, "Q", "operator.delete");
        h.shared.keymap.bind(mode, "<Space>e", "motion.line_end");
    }
    h.keys("Qw");
    assert_eq!(h.state(), "a |c\nd");
    h.keys("Q<space>e");
    assert_eq!(h.state(), "a| \nd");
    h.keys("QQ");
    assert_eq!(h.state(), "|d");

    // Unbound keys are dead, in every table they were removed from.
    let mut h = Harness::new("|abc");
    assert!(h.shared.keymap.unbind(MapMode::Normal, "x"));
    assert!(!h.shared.keymap.unbind(MapMode::Normal, "x"));
    h.keys("x");
    assert_eq!(h.state(), "|abc");
    h.shared.keymap.bind(MapMode::Normal, "x", "motion.right");
    h.keys("xdx");
    assert_eq!(h.state(), "a|bc");

    // An empty keymap leaves only typing — and no way out of insert mode.
    let mut h = Harness::new("|abc");
    h.shared.keymap = Keymap::empty();
    h.keys("xdd");
    assert_eq!(h.state(), "|abc");
    // But `:` opens the command line, for keys to be bound from there,
    // until the first binding of normal mode.
    h.keys(":");
    assert_eq!(h.vim.mode, Mode::Command);
    h.keys("<esc>");
    assert_eq!(h.vim.mode, Mode::Normal);
    h.shared
        .keymap
        .bind(MapMode::Normal, "x", "edit.delete_char");
    h.keys(":");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert!(h.shared.keymap.unbind(MapMode::Normal, "x"));
    h.shared.keymap.bind(MapMode::Normal, "i", "mode.insert");
    h.keys("ix<esc>");
    assert_eq!(h.vim.mode, Mode::Insert);
    assert_eq!(h.state(), "x|abc");
}

#[test]
fn shorter_binding_inside_a_longer_one() {
    // `space` moves right, but also starts a leader binding.
    let mut h = Harness::new("|abcd");
    h.shared
        .keymap
        .bind(MapMode::Normal, "<Space>f", "workspace.find_file");
    h.keys("<space>");
    assert_eq!(h.state(), "|abcd");
    assert_eq!(h.vim.pending_display(), "<Space>");
    assert!(h.vim.wants_timeout(&h.shared));
    h.keys("f");
    assert_eq!(h.effects, vec![command("workspace.find_file", None)]);
    assert!(!h.vim.has_pending());

    // Another key: `space` runs as itself, then that key.
    h.keys("<space>x");
    assert_eq!(h.state(), "a|cd");
    // No key at all: the host's timer settles it.
    h.keys("<space>");
    h.timeout();
    assert_eq!(h.state(), "ac|d");
    assert!(!h.vim.has_pending());

    // A prefix that is nothing by itself just waits; escape abandons it.
    h.keys("g");
    assert!(h.vim.has_pending() && !h.vim.wants_timeout(&h.shared));
    h.timeout();
    assert!(h.vim.has_pending());
    h.keys("<esc>x");
    assert_eq!(h.state(), "a|c");
}

#[test]
fn keys_mapped_to_keys() {
    let mut h = Harness::new("ab |cd ef\ngh");
    h.shared.keymap.bind_keys(MapMode::Normal, "Y", "y$");
    h.shared.keymap.bind_keys(MapMode::Normal, "Q", "dw");
    h.keys("YP");
    assert_eq!(h.state(), "ab cd e|fcd ef\ngh");
    // A count goes to the first mapped command; `.` repeats the mapping.
    h.keys("0Q.");
    assert_eq!(h.state(), "|efcd ef\ngh");
    h.keys("u2Q");
    assert_eq!(h.state(), "|ef\ngh");

    // The result is mapped again, so a mapping can lead back to itself.
    let mut h = Harness::new("|abc");
    h.shared.keymap.bind_keys(MapMode::Normal, "x", "lx");
    h.keys("x");
    assert_eq!(h.vim.message.as_deref(), Some("Key mapping never ends"));
    assert!(!h.vim.has_pending());
    assert_eq!(h.state(), "ab|c");
}

#[test]
fn insert_mode_mappings() {
    let mut h = Harness::new("|");
    h.shared.keymap.bind(MapMode::Insert, "jk", "insert.escape");
    h.shared
        .keymap
        .bind(MapMode::Insert, "<C-a>", "insert.home");
    h.keys("ijk");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(h.state(), "|");

    // `j` is held back until the next key shows it is not `jk`.
    h.keys("ij");
    assert_eq!(h.state(), "|");
    h.keys("a");
    assert_eq!(h.state(), "ja|");
    h.keys("jjk");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(h.state(), "ja|j");

    // …or until the host gives up waiting.
    h.keys("aj");
    assert!(h.vim.wants_timeout(&h.shared));
    h.timeout();
    assert_eq!(h.state(), "jajj|");
    h.keys("<c-a>x<esc>");
    assert_eq!(h.state(), "|xjajj");

    // Pasted text is not typing, and is never mapped.
    let mut cx = Ctx {
        buf: &mut h.buf,
        shared: &mut h.shared,
        clipboard: &mut h.clipboard,
        viewport: Viewport {
            top: 0,
            height: 10,
            scroll_off: 0,
        },
        lang: &h.lang,
        syntax: None,
    };
    h.vim.handle_key(Key::Char('i'), &mut cx);
    h.vim.paste("jk", &mut cx);
    assert_eq!(h.vim.mode, Mode::Insert);
    assert_eq!(h.buf.text(), "jkxjajj\n");
}

#[test]
fn every_builtin_command_runs() {
    // Bind each command to a key in each table and press it: none may
    // panic, wherever it is used.
    for name in command_names() {
        for mode in MapMode::ALL {
            let mut h = Harness::new("fn main() {\n    let |x = (1, \"two\");\n}\n\nend");
            h.shared.keymap.bind(mode, "<C-q>", name);
            let lead = match mode {
                MapMode::Normal => "",
                MapMode::Visual => "v",
                MapMode::Operator => "d",
                MapMode::Insert => "i",
            };
            h.keys(lead).keys("<c-q>").keys("w<esc><esc>");
        }
    }
}

#[test]
fn marks() {
    check("|a\nb\nc", "majjmb'a", "|a\nb\nc");
    check("a\n  b|x\nc", "mzgg`z", "a\n  b|x\nc");
    check("a\n  b|x\nc", "mzgg'z", "a\n  |bx\nc");
    // Where the last jump left from.
    check("|a\nb\nc", "G``", "|a\nb\nc");
    check("a\n|b\nc\nd", "majd'a", "a\n|d");
    check("|foo bar", "wmb0d`b", "|bar");
    // A mark stays on its text when lines come and go above it.
    check("|x\nfoo bar", "jwmaggOnew<Esc>`a", "new\nx\nfoo |bar");
    check("|x\nfoo bar", "jwmaggdd`a", "foo |bar");

    let mut h = Harness::new("|a");
    h.keys("'q");
    assert_eq!(h.vim.message.as_deref(), Some("Mark q is not set"));
}

#[test]
fn macros() {
    check("|a\nb\nc", "qqA!<Esc>jq@q@@", "a!\nb!\nc|!");
    check("|1\n2\n3\n4", "qaxjq3@a", "\n\n\n|");
    // `.` repeats the last change of the macro, not the macro.
    check("|ab cd ef", "qqdwq@q.", "|");

    let mut h = Harness::new("|ab cd");
    assert_eq!(h.vim.recording(), None);
    h.keys("qq");
    assert_eq!(h.vim.recording(), Some('q'));
    h.keys("dwq");
    assert_eq!(h.vim.recording(), None);
    // The keys are in the register, to look at and to put.
    assert_eq!(h.shared.register('q').unwrap().text, "dw");
    h.keys("u@q");
    assert_eq!(h.state(), "|cd");
    // Each command of a macro is undone by itself.
    check("|a b c d", "qqdwdwq@qu", "|d");

    // Keys yanked from the text run as a macro too.
    check("|dw\nfoo bar", "\"ayiwj@a", "dw\n|bar");
    // One that runs another.
    check("|a b c d", "qadwqqb@ax<Esc>q@b", "|");
    // One that runs itself is stopped.
    let mut h = Harness::new("|a");
    h.keys("qqa.<Esc>q");
    h.shared.registers_insert_for_test('q', "a.<Esc>@q");
    h.keys("@q");
    assert_eq!(h.vim.message.as_deref(), Some("Macro never ends"));
    assert_eq!(h.vim.mode, Mode::Normal);
}

#[test]
fn visual_block() {
    check("|abc\ndef\nghi", "<C-v>jld", "|c\nf\nghi");
    check("|abc\ndef\nghi", "l<C-v>jlhd", "a|c\ndf\nghi");
    check("|abc\ndef\nghi", "<C-v>jjIX<Esc>", "|Xabc\nXdef\nXghi");
    check("|ab\nc\ndef", "<C-v>jjlA!<Esc>", "ab|!\nc !\nde!f");
    check("|abc\ndef", "<C-v>jcX<Esc>", "|Xbc\nXef");
    check("|abc\ndef", "<C-v>jcX<Esc>u", "abc\n|def");
    check("|ab\ncd", "<C-v>jy$p", "ab|a\ncdc");
    check("|ab\ncd", "<C-v>jyGo<Esc>p", "ab\ncd\n|a\nc");
    check("|abc\ndef", "<C-v>jlrx", "|xxc\nxxf");
    check("|abc\ndef", "<C-v>jU", "|Abc\nDef");
    // A line the block does not reach gives it nothing.
    check("|abcd\nx\nefgh", "<C-v>jjlld", "|d\n\nh");
    check("ab|cd\nx\nefgh", "<C-v>jjIX<Esc>", "ab|Xcd\nx\nefXgh");
    check("|a\nb\nc", "<C-v>j>", "    |a\n    b\nc");

    let mut h = Harness::new("|abc\ndef");
    h.keys("<C-v>jl");
    assert_eq!(h.vim.mode, Mode::VisualBlock);
    assert_eq!(h.vim.mode.label(), "V-BLOCK");
    assert_eq!(h.vim.selections(&h.buf), [(0, 2), (4, 6)]);
    h.keys("v");
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.vim.selections(&h.buf), [(0, 6)]);
    h.keys("<C-v><C-v>");
    assert_eq!(h.vim.mode, Mode::Normal);
    h.keys("gv");
    assert_eq!(h.vim.mode, Mode::VisualBlock);

    // In the other visual modes `I` and `A` go to the ends of the selection.
    check("a |bcd e", "veIX<Esc>", "a |Xbcd e");
    check("a |bcd e", "veAX<Esc>", "a bcd|X e");
}

#[test]
fn surround() {
    check("|foo bar", "ysiw)", "|(foo) bar");
    check("|foo bar", "ysiw(", "|( foo ) bar");
    check("|foo bar", "ysiwb", "|(foo) bar");
    check("|foo bar", "ys$\"", "|\"foo bar\"");
    check("|foo bar", "ysiw*", "|*foo* bar");
    check("  fo|o bar", "yss]", "  |[foo bar]");
    check("(fo|o)", "ds)", "|foo");
    check("( fo|o )", "ds(", "|foo");
    check("a \"fo|o\" b", "ds\"", "a |foo b");
    check("\"fo|o\"", "cs\"'", "|'foo'");
    check("(fo|o)", "cs)]", "|[foo]");
    check("(fo|o)", "cs){", "|{ foo }");
    check("f(\n  |x\n)", "cs)]", "f|[\n  x\n]");
    check("|foo bar", "veS]", "|[foo] bar");
    check("  |foo\n  bar", "VjS}", "  |{\n  foo\n  bar\n  }");
    check("|ab\ncd", "<C-v>jS)", "|(a)b\n(c)d");
    // One change: to repeat, and to undo.
    check("|a b", "ysiw)W.", "(a) |(b)");
    check("|a b", "ysiw)u", "|a b");
    // Escape gives the operator up; nothing is left waiting.
    check("|foo", "ysiw<Esc>x", "|oo");
    check("|foo", "ds<Esc>x", "|oo");
    check("fo|o", "ds)", "fo|o");
    // The operators `s` comes after are what they were.
    check("|foo bar\nx", "yyp", "foo bar\n|foo bar\nx");
    check("|foo bar", "dw", "|bar");
    check("|foo bar", "cwx<Esc>", "|x bar");

    let mut h = Harness::new("|foo");
    h.keys("ysiw");
    assert_eq!(h.vim.pending_keys().unwrap().awaiting, Some(Awaiting::Char));
    assert_eq!(h.vim.pending_display(), "ysiw");
}

fn check_pairs(before: &str, keys: &str, after: &str) {
    let mut h = Harness::new(before);
    h.shared.auto_pairs = true;
    h.keys(keys);
    assert_eq!(h.state(), after, "keys: {keys}");
}

#[test]
fn auto_pairs() {
    check_pairs("|", "i(", "(|)");
    check_pairs("|", "i()", "()|");
    check_pairs("|", "i[{\"", "[{\"|\"}]");
    check_pairs("|", "i\"x\"", "\"x\"|");
    check_pairs("|", "if(x<Esc>", "f(|x)");
    // Not in front of a word, and an apostrophe is not a quote.
    check_pairs("|foo", "i(", "(|foo");
    check_pairs("do|n", "a't", "don't|");
    check_pairs("|", "i\\\"", "\\\"|");
    // Taking the opening one back takes both.
    check_pairs("|", "i(<BS>", "|");
    check_pairs("|", "i(x<BS><BS>", "|");
    check_pairs("(|)x", "i<BS>", "|x");
    check_pairs("|", "i{<CR>", "{\n    |\n}");
    // Repeated as it was typed.
    check_pairs("|\n\n", "ifoo(x<Esc>j.", "foo(x)\nfoo(|x)");
    // Pasted text is left alone.
    let mut h = Harness::new("|");
    h.shared.auto_pairs = true;
    h.keys("i");
    let mut cx = Ctx {
        buf: &mut h.buf,
        shared: &mut h.shared,
        clipboard: &mut h.clipboard,
        viewport: Viewport {
            top: 0,
            height: 10,
            scroll_off: 0,
        },
        lang: &h.lang,
        syntax: None,
    };
    h.vim.paste("f(", &mut cx);
    assert_eq!(h.buf.text(), "f(\n");
    // And only what the language pairs is paired.
    let mut h = Harness::new("|");
    h.shared.auto_pairs = true;
    h.lang.pairs.retain(|pair| pair.0 != '\'');
    h.keys("i<'a");
    assert_eq!(h.state(), "<'a|");
    // Off unless asked for.
    check("|", "i(", "(|");
}

/// A stand-in for a syntax tree: a word is in a line, a line in the text,
/// and every line is a "function".
struct Lines;

impl SyntaxTree for Lines {
    fn object(
        &mut self,
        buf: &Buffer,
        pos: usize,
        kind: char,
        around: bool,
    ) -> Option<(usize, usize)> {
        let line = buf.line_of(pos);
        let (start, end) = (motion::first_non_blank(buf, line), buf.line_end(line));
        match (kind, around) {
            ('f', true) => Some((start, end)),
            ('f', false) => Some((start + 1, end - 1)),
            _ => None,
        }
    }

    fn enclosing(&mut self, buf: &Buffer, start: usize, end: usize) -> Option<(usize, usize)> {
        let line = buf.line_of(start);
        let word = motion::word_under_cursor(buf, start)?;
        let line = (buf.line_start(line), buf.line_end(line));
        let all = (0, buf.max_offset());
        [word, line, all]
            .into_iter()
            .find(|&(s, e)| s <= start && end <= e && (s, e) != (start, end))
    }

    fn neighbor(&mut self, buf: &Buffer, pos: usize, kind: char, forward: bool) -> Option<usize> {
        let line = buf.line_of(pos);
        match (kind, forward) {
            ('f', true) if line < buf.last_line() => Some(buf.line_start(line + 1)),
            ('f', false) if line > 0 => Some(buf.line_start(line - 1)),
            _ => None,
        }
    }
}

fn with_syntax(text: &str) -> Harness {
    let mut h = Harness::new(text);
    h.syntax = Some(Box::new(Lines));
    h
}

#[test]
fn syntax_objects_and_motions() {
    let mut h = with_syntax("a\n  [f|oo]\nb");
    h.keys("dif");
    assert_eq!(h.state(), "a\n  [|]\nb");
    // A whole one that has its lines to itself goes with them.
    let mut h = with_syntax("a\n  [f|oo]\nb");
    h.keys("daf");
    assert_eq!(h.state(), "a\n|b");
    let mut h = with_syntax("a\n  [f|oo]\nb");
    h.keys("vaf");
    assert_eq!(h.vim.selection(&h.buf), Some((2, 9)));

    let mut h = with_syntax("|a\nb\nc");
    h.keys("2]f");
    assert_eq!(h.state(), "a\nb\n|c");
    h.keys("[f");
    assert_eq!(h.state(), "a\n|b\nc");
    h.keys("d]f");
    assert_eq!(h.state(), "a\n|\nc");
    // `''` goes back over one of these jumps.
    h.keys("gg]f''");
    assert_eq!(h.state(), "|a\n\nc");

    // With no syntax tree they do nothing, and leave nothing pending.
    check("|foo(a)", "difx", "|oo(a)");
    check("|a\nb", "]fx", "|\nb");
}

#[test]
fn expands_and_shrinks_the_selection() {
    let mut h = with_syntax("foo b|ar baz\nqux");
    h.keys("]x");
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.vim.selection(&h.buf), Some((4, 7)));
    h.keys("]x");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 11)));
    h.keys("]x");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 15)));
    h.keys("[x");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 11)));
    h.keys("[x[x");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(h.state(), "foo b|ar baz\nqux");
    // A selection moved by hand has no steps back.
    h.keys("]xl[x");
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.vim.selection(&h.buf), Some((4, 8)));

    let mut h = Harness::new("|foo");
    h.keys("]x");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(
        h.vim.message.as_deref(),
        Some("No syntax tree for this file")
    );
}

/// What flash does, on the keys med gives it: `s`, and `z` after an
/// operator, to go to text; `S` for a syntax node; `r` after an operator
/// for it to work elsewhere; and `f` `F` `t` `T` on every line on screen.
fn bind_flash(h: &mut Harness) {
    let keymap = &mut h.shared.keymap;
    for mode in [MapMode::Normal, MapMode::Visual] {
        keymap.bind(mode, "s", "motion.flash");
    }
    keymap.bind(MapMode::Operator, "z", "motion.flash");
    keymap.bind(MapMode::Operator, "r", "motion.flash_remote");
    for mode in [MapMode::Normal, MapMode::Operator] {
        keymap.bind(mode, "S", "select.flash");
    }
    for mode in [MapMode::Normal, MapMode::Visual, MapMode::Operator] {
        keymap.bind(mode, "f", "motion.flash_find_forward");
        keymap.bind(mode, "F", "motion.flash_find_backward");
        keymap.bind(mode, "t", "motion.flash_till_forward");
        keymap.bind(mode, "T", "motion.flash_till_backward");
    }
}

fn flash_harness(text: &str) -> Harness {
    let mut h = Harness::new(text);
    bind_flash(&mut h);
    h
}

fn flash_labels(h: &Harness) -> Vec<(usize, Option<char>)> {
    let found = h.vim.flash().expect("flash is looking for text");
    found.matches.iter().map(|m| (m.start, m.label)).collect()
}

#[test]
fn flash_goes_to_a_label() {
    let mut h = flash_harness("|one two\nthree two\n");
    h.keys("s");
    assert_eq!(flash_labels(&h), []);
    assert_eq!(
        h.vim.pending_keys().and_then(|pending| pending.awaiting),
        Some(Awaiting::Flash)
    );
    // The nearest first; `w` and `h` come after a `t`, so are no labels.
    h.keys("t");
    assert_eq!(
        flash_labels(&h),
        [(4, Some('a')), (8, Some('s')), (14, Some('d'))]
    );
    // A place keeps its label as the text gets longer.
    h.keys("w");
    assert_eq!(flash_labels(&h), [(4, Some('a')), (14, Some('d'))]);
    assert_eq!(h.vim.pending_display(), "stw");
    assert_eq!(h.state(), "|one two\nthree two");

    h.keys("d");
    assert_eq!(h.state(), "one two\nthree |two");
    assert!(h.vim.flash().is_none());
    assert!(!h.vim.has_pending());
    // A jump, to come back from.
    assert_eq!(h.effects, [Effect::Jump { from: 0 }]);
    assert_eq!(h.keys("``").state(), "|one two\nthree two");
}

#[test]
fn flash_keys_that_are_not_text() {
    // Enter: the first place after the cursor.
    let mut h = flash_harness("one |two\nthree two\n");
    assert_eq!(h.keys("stw<CR>").state(), "one two\nthree |two");
    // And around to the first there is.
    assert_eq!(h.keys("son<CR>").state(), "|one two\nthree two");

    // Backspace takes a character back, and with none left gives up.
    h.keys("stw<BS>");
    assert_eq!(h.vim.pending_display(), "st");
    assert_eq!(flash_labels(&h).len(), 3);
    h.keys("<BS>");
    assert_eq!(h.vim.pending_display(), "s");
    assert_eq!(flash_labels(&h), []);
    h.keys("<BS>");
    assert!(!h.vim.has_pending());
    assert_eq!(h.state(), "|one two\nthree two");

    // Escape gives up, and leaves a selection as it was.
    h.keys("vstw<Esc>");
    assert!(!h.vim.has_pending());
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.keys("<Esc>").state(), "|one two\nthree two");

    // Text that is nowhere on screen gives up as well.
    h.keys("stq");
    assert!(!h.vim.has_pending());
    assert_eq!(h.vim.message.as_deref(), Some("Pattern not found: tq"));
    assert_eq!(h.state(), "|one two\nthree two");
    // A digit is text like any other, not a count.
    let mut h = flash_harness("|a1 b2 c2\n");
    h.keys("s2");
    assert_eq!(flash_labels(&h), [(4, Some('a')), (7, Some('s'))]);
    assert_eq!(h.keys("s").state(), "a1 b2 c|2");
}

#[test]
fn flash_is_a_motion() {
    // After an operator, up to and with the first character found.
    let mut h = flash_harness("|one two three\n");
    h.keys("dzth");
    assert_eq!(flash_labels(&h), [(8, Some('s'))]);
    assert_eq!(h.keys("s").state(), "|hree");
    // Backwards too.
    let mut h = flash_harness("one two |three\n");
    assert_eq!(h.keys("dzons").state(), "|hree");
    // Nothing is done when it gives up.
    let mut h = flash_harness("|one two three\n");
    assert_eq!(h.keys("dzth<Esc>").state(), "|one two three");
    assert!(!h.vim.has_pending());

    // In visual mode the selection goes there.
    let mut h = flash_harness("|one two three\n");
    h.keys("vsths");
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.vim.selection(&h.buf), Some((0, 9)));
    assert_eq!(h.keys("d").state(), "|hree");

    // Unbound, `s` is vim's own.
    check("|abc", "sx<esc>", "|xbc");
}

#[test]
fn finds_on_every_line_on_screen() {
    let mut h = flash_harness("|ab xb\ncb db\n");
    assert_eq!(h.keys("fb").state(), "a|b xb\ncb db");
    assert!(!h.vim.has_pending());
    // Where it could go next is marked, and the text it looked through
    // from here on is what a host dims.
    let view = h.vim.flash().expect("the find left its marks");
    assert_eq!(flash_labels(&h), [(4, None), (7, None), (10, None)]);
    assert_eq!((view.dim, view.nodes), ((2, 11), false));

    // Its key again goes on, onto the next line; the key that looks the
    // other way goes back.
    assert_eq!(h.keys("f").state(), "ab x|b\ncb db");
    assert_eq!(h.keys("f").state(), "ab xb\nc|b db");
    assert_eq!(flash_labels(&h), [(10, None)]);
    assert_eq!(h.keys("F").state(), "ab x|b\ncb db");
    assert_eq!(flash_labels(&h), [(7, None), (10, None)]);
    // `;` and `,` do the same, and a count is how many to go on by.
    assert_eq!(h.keys(";").state(), "ab xb\nc|b db");
    assert_eq!(h.keys(",").state(), "ab x|b\ncb db");
    assert_eq!(h.keys("2f").state(), "ab xb\ncb d|b");
    // Any other key and the marks are gone: `f` is a new find again.
    h.keys("h");
    assert!(h.vim.flash().is_none());
    h.keys("f");
    assert!(h.vim.has_pending());
    assert_eq!(h.keys("b").state(), "ab xb\ncb d|b");
    assert_eq!(flash_labels(&h), []);

    // Backwards the key goes on backwards, and the other one forwards.
    let mut h = flash_harness("ab xb\ncb |db\n");
    assert_eq!(h.keys("Fb").state(), "ab xb\nc|b db");
    assert_eq!(flash_labels(&h), [(1, None), (4, None)]);
    assert_eq!(h.vim.flash().map(|view| view.dim), Some((0, 7)));
    assert_eq!(h.keys("f").state(), "ab x|b\ncb db");
    assert_eq!(h.keys("F").state(), "ab xb\nc|b db");

    // `t` stops short, and goes on past the one it is next to.
    let mut h = flash_harness("|ab xb\ncb db\n");
    assert_eq!(h.keys("tb").state(), "|ab xb\ncb db");
    assert_eq!(h.keys("t").state(), "ab |xb\ncb db");
    assert_eq!(h.keys("t").state(), "ab xb\n|cb db");
    // Another find's key is a new find.
    h.keys("f");
    assert!(h.vim.has_pending());
    assert_eq!(h.keys("d").state(), "ab xb\ncb |db");

    // After an operator it is done with, and leaves nothing marked.
    let mut h = flash_harness("|ab\ncd ef\n");
    assert_eq!(h.keys("dfd").state(), "| ef");
    assert!(h.vim.flash().is_none());
    // In visual mode the selection goes along.
    let mut h = flash_harness("|ab\ncd ef\n");
    h.keys("vfdf");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 5)));
    let mut h = flash_harness("|ab\ncd ed\n");
    h.keys("vfdf");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 8)));

    // Unbound, `f` stays on its line.
    check("|ab\ncd", "fd", "|ab\ncd");
}

#[test]
fn selects_a_syntax_node_by_its_label() {
    let mut h = with_syntax("foo b|ar baz\nqux");
    bind_flash(&mut h);
    h.keys("S");
    // The nodes around the cursor, from the smallest out: nothing is
    // dimmed, and the labels go beside the nodes.
    let view = h.vim.flash().expect("the nodes are labelled");
    assert_eq!((view.dim, view.nodes), ((0, 0), true));
    let nodes: Vec<_> = view
        .matches
        .iter()
        .map(|m| (m.start, m.end, m.label))
        .collect();
    assert_eq!(
        nodes,
        [(4, 7, Some('a')), (0, 11, Some('s')), (0, 15, Some('d'))]
    );
    assert_eq!(h.vim.pending_display(), "S");

    h.keys("s");
    assert_eq!(h.vim.mode, Mode::Visual);
    assert_eq!(h.vim.selection(&h.buf), Some((0, 11)));
    assert!(h.vim.flash().is_none());
    // A step like `]x`'s: `[x` leads back from it, `]x` further out.
    h.keys("]x");
    assert_eq!(h.vim.selection(&h.buf), Some((0, 15)));
    h.keys("[x[x");
    assert_eq!(h.vim.mode, Mode::Normal);
    assert_eq!(h.state(), "foo b|ar baz\nqux");

    // Enter is the smallest; a key that is no label gives up.
    h.keys("S<CR>");
    assert_eq!(h.vim.selection(&h.buf), Some((4, 7)));
    h.keys("<Esc>Sx");
    assert!(!h.vim.has_pending());
    assert_eq!(h.vim.mode, Mode::Normal);

    // After an operator, the node is what it acts on.
    assert_eq!(h.keys("bdSa").state(), "foo | baz\nqux");
    // On the blank the smallest is the line.
    assert_eq!(h.keys("dSa").state(), "|\nqux");

    let mut h = flash_harness("|foo");
    h.keys("S");
    assert!(!h.vim.has_pending());
    assert_eq!(
        h.vim.message.as_deref(),
        Some("No syntax tree for this file")
    );
}

#[test]
fn an_operator_works_elsewhere_and_comes_back() {
    // `yr`, the place, and there the motion: the cursor ends where it was.
    let mut h = flash_harness("o|ne two\nthree four\n");
    h.keys("yrf");
    assert_eq!(flash_labels(&h), [(14, Some('a'))]);
    h.keys("a");
    assert_eq!(h.state(), "one two\nthree |four");
    assert_eq!(
        h.vim.pending_keys().map(|pending| pending.mode),
        Some(MapMode::Operator)
    );
    h.keys("iw");
    assert_eq!(h.state(), "o|ne two\nthree four");
    assert_eq!(h.clipboard.0.as_deref(), Some("four"));
    assert!(!h.vim.has_pending());

    // Where it was moves with the text taken away before it.
    let mut h = flash_harness("one two\nthree fo|ur\n");
    assert_eq!(h.keys("drtsiw").state(), "one \nthree fo|ur");
    // A change comes back when the typing is done.
    let mut h = flash_harness("o|ne two\nthree four\n");
    h.keys("crfaiwx");
    assert_eq!(h.state(), "one two\nthree x|");
    assert_eq!(h.keys("<Esc>").state(), "o|ne two\nthree x");
    // Given up on, there or before, it comes back as well.
    assert_eq!(h.keys("yrta<Esc>").state(), "o|ne two\nthree x");
    assert_eq!(h.keys("yrt<Esc>").state(), "o|ne two\nthree x");
    assert!(!h.vim.has_pending());
    // The node there, with `S`.
    let mut h = with_syntax("o|ne two\nthree four\n");
    bind_flash(&mut h);
    assert_eq!(h.keys("drfaSa").state(), "o|ne two\nthree ");
}
