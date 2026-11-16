use super::*;
use std::time::{Duration, Instant};

fn mods(control: bool, alt: bool, shift: bool) -> Modifiers {
    Modifiers {
        control,
        alt,
        shift,
    }
}

const NONE: Modifiers = Modifiers {
    control: false,
    alt: false,
    shift: false,
};

#[test]
fn keys_as_xterm_sends_them() {
    let cases: &[(&str, Modifiers, bool, Option<&[u8]>)] = &[
        // Text comes by the input method.
        ("a", NONE, false, None),
        ("a", mods(false, false, true), false, None),
        ("space", NONE, false, None),
        ("a", mods(true, false, false), false, Some(b"\x01")),
        ("c", mods(true, false, false), false, Some(b"\x03")),
        ("z", mods(true, false, true), false, Some(b"\x1a")),
        ("[", mods(true, false, false), false, Some(b"\x1b")),
        ("\\", mods(true, false, false), false, Some(b"\x1c")),
        ("space", mods(true, false, false), false, Some(b"\0")),
        ("b", mods(false, true, false), false, Some(b"\x1bb")),
        ("b", mods(false, true, true), false, Some(b"\x1bB")),
        ("x", mods(true, true, false), false, Some(b"\x1b\x18")),
        ("enter", NONE, false, Some(b"\r")),
        ("enter", mods(false, true, false), false, Some(b"\x1b\r")),
        ("backspace", NONE, false, Some(b"\x7f")),
        (
            "backspace",
            mods(false, true, false),
            false,
            Some(b"\x1b\x7f"),
        ),
        ("tab", NONE, false, Some(b"\t")),
        ("tab", mods(false, false, true), false, Some(b"\x1b[Z")),
        ("escape", NONE, false, Some(b"\x1b")),
        ("up", NONE, false, Some(b"\x1b[A")),
        ("up", NONE, true, Some(b"\x1bOA")),
        ("left", mods(true, false, false), true, Some(b"\x1b[1;5D")),
        ("right", mods(false, true, true), false, Some(b"\x1b[1;4C")),
        ("home", NONE, false, Some(b"\x1b[H")),
        ("end", NONE, true, Some(b"\x1bOF")),
        ("delete", NONE, false, Some(b"\x1b[3~")),
        (
            "pageup",
            mods(false, false, true),
            false,
            Some(b"\x1b[5;2~"),
        ),
        ("f1", NONE, false, Some(b"\x1bOP")),
        ("f4", mods(true, false, false), false, Some(b"\x1b[1;5S")),
        ("f5", NONE, false, Some(b"\x1b[15~")),
        ("f6", NONE, false, Some(b"\x1b[17~")),
        ("f10", NONE, false, Some(b"\x1b[21~")),
        ("f11", NONE, false, Some(b"\x1b[23~")),
        ("f12", mods(false, false, true), false, Some(b"\x1b[24;2~")),
        ("f13", NONE, false, None),
        ("capslock", NONE, false, None),
    ];
    for (key, mods, app_cursor, want) in cases {
        let got = encode_key(key, *mods, *app_cursor);
        assert_eq!(
            got.as_deref(),
            *want,
            "{key} {mods:?} app_cursor={app_cursor}"
        );
    }
}

#[test]
fn colors_by_number() {
    let palette = Palette::dark();
    assert_eq!(palette.indexed(1), palette.ansi[1]);
    assert_eq!(palette.indexed(16), Rgb::new(0, 0, 0));
    assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
    assert_eq!(palette.indexed(231), Rgb::new(255, 255, 255));
    assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
    assert_eq!(palette.indexed(255), Rgb::new(238, 238, 238));
}

/// Wait for the program to be done, and its screen.
fn run(script: &str, cols: u16, rows: u16) -> (Screen, Option<i32>) {
    let (terminal, mut events) = Terminal::spawn(Options {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cols,
        rows,
        ..Options::default()
    })
    .expect("a pty");
    let deadline = Instant::now() + Duration::from_secs(10);
    let code = loop {
        assert!(Instant::now() < deadline, "the program did not exit");
        match events.try_recv() {
            Ok(Event::Exited(code)) => break code,
            Ok(_) => {}
            Err(mpsc::TryRecvError::Closed) => panic!("no more events, and no exit"),
            Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    (terminal.screen(&Palette::dark()), code)
}

#[test]
fn a_program_draws_on_the_screen() {
    let (screen, code) = run("printf 'hello\\r\\n\\033[31mred\\033[0m'; exit 3", 20, 4);
    assert_eq!((screen.cols, screen.rows), (20, 4));
    assert_eq!(screen.line_text(0), "hello");
    assert_eq!(screen.line_text(1), "red");
    assert_eq!(screen.lines[1][0].fg, Palette::dark().ansi[1]);
    assert_eq!(screen.lines[0][0].fg, Palette::dark().foreground);
    assert_eq!(code, Some(3));
}

#[test]
fn a_wide_character_takes_two_columns() {
    let (screen, _) = run("printf '中文ab'", 20, 2);
    let cells: Vec<(usize, char, bool)> = screen.lines[0]
        .iter()
        .take(4)
        .map(|cell| (cell.col, cell.c, cell.wide))
        .collect();
    assert_eq!(
        cells,
        [
            (0, '中', true),
            (2, '文', true),
            (4, 'a', false),
            (5, 'b', false)
        ]
    );
}

#[test]
fn the_program_runs_where_it_is_told_with_what_it_is_given() {
    let dir = std::env::temp_dir().canonicalize().expect("a temp dir");
    let (terminal, mut events) = Terminal::spawn(Options {
        program: Some("/bin/sh".into()),
        args: vec![
            "-c".into(),
            "printf '%s %s' \"$(pwd -P)\" \"$MED_TERM_TEST\"".into(),
        ],
        cwd: Some(dir.clone()),
        env: HashMap::from([("MED_TERM_TEST".into(), "given".into())]),
        cols: 200,
        rows: 2,
    })
    .expect("a pty");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(events.try_recv(), Ok(Event::Exited(_))) {
        assert!(Instant::now() < deadline, "the program did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let screen = terminal.screen(&Palette::dark());
    assert_eq!(screen.line_text(0), format!("{} given", dir.display()));
}

#[test]
fn resizing_changes_the_screen_and_tells_the_program() {
    let (mut terminal, _events) = Terminal::spawn(Options {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), "sleep 5".into()],
        cols: 10,
        rows: 3,
        ..Options::default()
    })
    .expect("a pty");
    assert!(!terminal.resize(10, 3, 8, 16));
    assert!(terminal.resize(40, 6, 8, 16));
    let screen = terminal.screen(&Palette::dark());
    assert_eq!((screen.cols, screen.rows, screen.lines.len()), (40, 6, 6));
    assert_eq!(terminal.size(), (40, 6));
}

/// Start `script` in a terminal, and wait for it to be done.
fn finished(script: &str, cols: u16, rows: u16) -> Terminal {
    let (terminal, mut events) = Terminal::spawn(Options {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cols,
        rows,
        ..Options::default()
    })
    .expect("a pty");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(events.try_recv(), Ok(Event::Exited(_))) {
        assert!(Instant::now() < deadline, "the program did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    terminal
}

#[test]
fn the_history_has_what_went_by_and_where_the_cursor_is() {
    // Six lines on a screen of three: three went by.
    let terminal = finished("printf '1\\n2\\n3\\n4\\n5\\n中文x'", 10, 3);
    let history = terminal.history();
    assert_eq!(history.text, "1\n2\n3\n4\n5\n中文x");
    // After the `x`: the fourth character, though the sixth column.
    assert_eq!((history.line, history.col), (5, 3));

    // Looking back, the cursor goes to the top of what is shown.
    terminal.scroll(2);
    let history = terminal.history();
    assert_eq!((history.line, history.col), (1, 0));
}

#[test]
fn the_mouse_selects_by_cell_word_and_line() {
    let terminal = finished("printf 'hello world\\nsecond line'", 20, 3);
    assert_eq!(terminal.selection_text(), None);
    terminal.select_start(0, 1, false, SelectKind::Simple);
    terminal.select_update(0, 3, true);
    assert_eq!(terminal.selection_text().as_deref(), Some("ell"));
    let screen = terminal.screen(&Palette::dark());
    let selected: Vec<bool> = screen.lines[0][..5].iter().map(|c| c.selected).collect();
    assert_eq!(selected, [false, true, true, true, false]);

    terminal.select_start(0, 8, false, SelectKind::Word);
    assert_eq!(terminal.selection_text().as_deref(), Some("world"));
    terminal.select_start(0, 2, false, SelectKind::Line);
    terminal.select_update(1, 0, false);
    // Whole lines, each with its end.
    assert_eq!(
        terminal.selection_text().as_deref(),
        Some("hello world\nsecond line\n")
    );
    terminal.clear_selection();
    assert_eq!(terminal.selection_text(), None);
}
