//! Headless end-to-end tests: a real `Workspace` in a test window, driven by
//! simulated keystrokes. The panels are the ones in `examples/`, loaded as
//! an init file would load them.

use crate::keymap::PanelKeymaps;
use crate::user_panel::UserPanels;
use crate::workspace::Workspace;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use med_vim::{MapMode, Mode};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A scratch project directory, removed on drop.
struct Project(PathBuf);

impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "med-ui-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        for (name, text) in files {
            let path = dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_init(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.git(&["config", "user.name", "Test"]);
        self.git(&["config", "user.email", "test@example.invalid"]);
        self.git(&["config", "commit.gpgsign", "false"]);
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const VIM: &str = include_str!("../../../examples/vim.mlisp");
const KEYS: &str = include_str!("../../../examples/keys.mlisp");
const TREE: &str = include_str!("../../../examples/tree.mlisp");
const PICKER: &str = include_str!("../../../examples/picker.mlisp");
const GIT: &str = include_str!("../../../examples/git.mlisp");
const WHICH_KEY: &str = include_str!("../../../examples/which-key.mlisp");
const TODO: &str = include_str!("../../../examples/todo.mlisp");
const DIRED: &str = include_str!("../../../examples/dired.mlisp");
const OIL: &str = include_str!("../../../examples/oil.mlisp");
const STATUSLINE: &str = include_str!("../../../examples/statusline.mlisp");
const TITLEBAR: &str = include_str!("../../../examples/titlebar.mlisp");
const RSS: &str = include_str!("../../../examples/rss.mlisp");

/// How tall the title bar is, which the window's other bars and panels are
/// below.
const TITLE: f32 = 34.; // gpui::component::TITLE_BAR_HEIGHT

fn open<'a>(
    project: &Project,
    files: &[&str],
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    open_with(&[], project, files, cx)
}

/// The keys of `examples/`, which every test types: med binds none.
fn bind_example_keys(cx: &mut gpui::App) {
    for program in [VIM, KEYS] {
        if let Err(error) = crate::script::eval(program, cx) {
            panic!("{error}");
        }
    }
}

/// Open a window after the example keys and `programs` have run, as an
/// init file would have.
fn open_with<'a>(
    programs: &[&str],
    project: &Project,
    files: &[&str],
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    open_window(true, programs, project, files, cx)
}

fn open_window<'a>(
    keys: bool,
    programs: &[&str],
    project: &Project,
    files: &[&str],
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::init(cx);
        // Language servers wake tasks from their own threads, which the
        // deterministic test executor rejects; tests/lsp.rs covers them.
        cx.global_mut::<crate::Settings>().lsp = false;
        if keys {
            bind_example_keys(cx);
        }
        for program in programs {
            if let Err(error) = crate::script::eval(program, cx) {
                panic!("{error}");
            }
        }
    });
    let root = project.0.clone();
    let files: Vec<PathBuf> = files.iter().map(|f| project.path(f)).collect();
    let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(root, files, window, cx));
    cx.run_until_parked();
    (workspace, cx)
}

fn text(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    workspace.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).buf.text())
}

fn mode(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Mode {
    workspace.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).vim.mode)
}

/// (line, column), 1-based.
fn cursor(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> (usize, usize) {
    workspace.read_with(cx, |ws, cx| {
        ws.active_editor().unwrap().read(cx).cursor_position()
    })
}

fn active_file(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<PathBuf> {
    workspace.read_with(cx, |ws, cx| {
        ws.active_editor().and_then(|e| e.read(cx).path.clone())
    })
}

/// What a form comes to, as text: for looking at what a program keeps.
fn lisp(source: &str, cx: &mut VisualTestContext) -> String {
    match cx.update(|_, cx| crate::script::eval(source, cx)) {
        Ok(value) => value.to_string(),
        Err(error) => panic!("{source}: {error}"),
    }
}

fn shown(panel: &str, cx: &mut VisualTestContext) -> bool {
    let ws = workspace_id(cx);
    cx.update(|_, cx| cx.global::<UserPanels>().is_shown(panel, ws))
}

/// The workspace in the window `cx` is of, by its id.
fn workspace_id(cx: &mut VisualTestContext) -> Option<gpui::EntityId> {
    cx.update(|window, cx| {
        let here = window.window_handle();
        let all = crate::workspace::all(cx);
        let found = all.into_iter().find(|(window, _)| *window == here);
        found.map(|(_, workspace)| workspace.entity_id())
    })
}

/// Wait on real time for something that happens off the test executor
/// (git subprocesses are synchronous, language servers are not).
fn wait_for(
    cx: &mut VisualTestContext,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) -> bool {
    cx.executor().allow_parking();
    for _ in 0..400 {
        cx.run_until_parked();
        if done(cx) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[gpui::test]
fn edits_with_vim_keys_and_saves(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.rs", "fn main() {\n    one();\n    two();\n}\n")]);
    let (ws, cx) = open(&project, &["a.rs"], cx);
    assert_eq!(mode(&ws, cx), Mode::Normal);

    // Normal-mode commands.
    cx.simulate_keystrokes("j d d");
    assert_eq!(text(&ws, cx), "fn main() {\n    two();\n}\n");
    assert_eq!(cursor(&ws, cx), (2, 5));

    // Insert mode: text arrives through the input handler, like real typing.
    cx.simulate_keystrokes("shift-o t h r e e ( ) ; escape");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert_eq!(text(&ws, cx), "fn main() {\n    three();\n    two();\n}\n");

    // Undo, redo, dot-repeat, counts.
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "fn main() {\n    two();\n}\n");
    cx.simulate_keystrokes("ctrl-r");
    assert_eq!(text(&ws, cx), "fn main() {\n    three();\n    two();\n}\n");
    cx.simulate_keystrokes("g g c w p u b space f n escape");
    assert_eq!(
        text(&ws, cx),
        "pub fn main() {\n    three();\n    two();\n}\n"
    );

    // Visual mode + operator.
    cx.simulate_keystrokes("j shift-v j d");
    assert_eq!(text(&ws, cx), "pub fn main() {\n}\n");

    // Ex command line: also typed through the input handler.
    assert!(ws.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).is_dirty()));
    cx.simulate_keystrokes(": w enter");
    assert_eq!(project.read("a.rs"), "pub fn main() {\n}\n");
    assert!(!ws.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).is_dirty()));

    // Search and substitute.
    cx.simulate_keystrokes("/ m a i n enter");
    assert_eq!(cursor(&ws, cx), (1, 8));
    cx.simulate_keystrokes(": % s / m a i n / r u n / enter");
    assert_eq!(text(&ws, cx), "pub fn run() {\n}\n");
}

#[gpui::test]
fn refuses_to_drop_unsaved_changes(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let (ws, cx) = open(&project, &["a.txt", "b.txt"], cx);
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs().len()), 2);

    // gt / gT switch tabs.
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    cx.simulate_keystrokes("g t");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.simulate_keystrokes("g shift-t");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    cx.simulate_keystrokes("x : q enter");
    assert_eq!(
        ws.read_with(cx, |ws, _| ws.tabs().len()),
        2,
        ":q must not close a dirty tab"
    );
    cx.simulate_keystrokes(": q ! enter");
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs().len()), 1);
    assert_eq!(project.read("a.txt"), "one\n");

    // :e opens (or creates) a file relative to the project root.
    cx.simulate_keystrokes(": e space n e w . t x t enter");
    assert_eq!(active_file(&ws, cx), Some(project.path("new.txt")));
    cx.simulate_keystrokes("i h i escape : w enter");
    assert_eq!(project.read("new.txt"), "hi\n");
}

#[gpui::test]
fn finds_files_and_searches_the_project(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {\n    helper();\n}\n"),
        (
            "src/util/helper.rs",
            "pub fn helper() {\n    // the needle is here\n}\n",
        ),
        ("README.md", "# demo\n"),
    ]);
    let (ws, cx) = open_with(&[PICKER], &project, &["README.md"], cx);

    // Leader key opens the fuzzy finder; typing filters; enter opens.
    assert!(!shown("picker", cx));
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    assert!(shown("picker", cx));
    assert_eq!(lisp("(length picker-items)", cx), "3");
    cx.simulate_input("uhelp");
    cx.run_until_parked();
    assert_eq!(lisp("picker-query", cx), "\"uhelp\"");
    assert_eq!(
        lisp("(map (fn (item) (get item :label)) picker-items)", cx),
        "(\"helper.rs\")"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!shown("picker", cx));
    assert_eq!(
        active_file(&ws, cx),
        Some(project.path("src/util/helper.rs"))
    );
    // The keyboard is the editor's again.
    cx.simulate_keystrokes("g g");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // ctrl-n and ctrl-p move through the list, round its ends, and what
    // was typed last time is gone.
    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert_eq!(lisp("(list picker-query picker-selected)", cx), "(\"\" 0)");
    cx.simulate_keystrokes("ctrl-n ctrl-n");
    cx.run_until_parked();
    assert_eq!(lisp("picker-selected", cx), "2");
    cx.simulate_keystrokes("ctrl-n ctrl-p down");
    cx.run_until_parked();
    assert_eq!(lisp("picker-selected", cx), "0");

    // Escape closes the finder and hands focus back to the editor.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("picker", cx));
    cx.simulate_keystrokes("shift-g");
    assert_eq!(cursor(&ws, cx), (3, 1));
    cx.simulate_keystrokes("g g");

    // Project search jumps to the matching line and column.
    cx.simulate_keystrokes("space /");
    cx.run_until_parked();
    cx.simulate_input("needle");
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(
        lisp("(get (first picker-items) :detail)", cx),
        "\"src/util/helper.rs:2\""
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        active_file(&ws, cx),
        Some(project.path("src/util/helper.rs"))
    );
    assert_eq!(cursor(&ws, cx), (2, 12));

    // ctrl-o returns to where the search started.
    cx.simulate_keystrokes("ctrl-o");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // A list of places, as the references to a name are, is chosen from
    // in the same popup.
    let places = format!(
        "(list (list :path {a:?} :file \"src/main.rs\" :line 2 :col 5 :text \"helper();\")
               (list :path {b:?} :file \"README.md\" :line 1 :col 3 :text \"demo\"))",
        a = project.path("src/main.rs").display().to_string(),
        b = project.path("README.md").display().to_string(),
    );
    let places = cx.update(|_, cx| crate::script::eval(&places, cx).unwrap());
    cx.update(|window, cx| {
        ws.update(cx, |_, cx| {
            assert!(crate::script::run_hook(
                "locations",
                vec![places],
                window,
                cx
            ));
        })
    });
    cx.run_until_parked();
    assert!(shown("picker", cx));
    assert_eq!(lisp("(length picker-items)", cx), "2");
    cx.simulate_input("demo");
    cx.run_until_parked();
    assert_eq!(lisp("(length picker-items)", cx), "1");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("README.md")));
    assert_eq!(cursor(&ws, cx), (1, 3));
}

#[gpui::test]
fn navigates_the_file_tree(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("dir/inner.txt", "inner\n"),
        ("z.txt", "z\n"),
    ]);
    let (ws, cx) = open_with(&[TREE], &project, &["z.txt"], cx);
    let names =
        |cx: &mut VisualTestContext| lisp("(map (fn (entry) (get entry :name)) tree-entries)", cx);

    // Directories sort first: dir/, a.txt, z.txt. The tree is put away
    // until space e puts it up; the open file is revealed.
    assert_eq!(names(cx), r#"("dir" "a.txt" "z.txt")"#);
    assert!(!shown("tree", cx));
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert!(shown("tree", cx));
    assert_eq!(lisp("(get (tree-entry) :name)", cx), "\"z.txt\"");
    cx.simulate_keystrokes("g g l");
    cx.run_until_parked();
    assert_eq!(names(cx), r#"("dir" "inner.txt" "a.txt" "z.txt")"#);
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("dir/inner.txt")));

    // Opening a file moved focus to the editor, so vim keys edit again.
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "nner\n");

    // h on a file goes to its directory, and on that folds it; G is the
    // last row, and space e from the tree puts it away, back to the editor.
    cx.simulate_keystrokes("space e h");
    cx.run_until_parked();
    assert_eq!(lisp("(get (tree-entry) :name)", cx), "\"dir\"");
    cx.simulate_keystrokes("h shift-g");
    cx.run_until_parked();
    assert_eq!(names(cx), r#"("dir" "a.txt" "z.txt")"#);
    assert_eq!(lisp("(get (tree-entry) :name)", cx), "\"z.txt\"");
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert!(!shown("tree", cx));
    // q puts it away as well.
    cx.simulate_keystrokes("space e q");
    cx.run_until_parked();
    assert!(!shown("tree", cx));
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "ner\n");

    // A file that appears on disk is in the tree once the editor has
    // reason to look: here, a file being written. Going to the tree just
    // now revealed the open file, so dir/ is unfolded again.
    fs::write(project.path("b.txt"), "b\n").unwrap();
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    assert_eq!(names(cx), r#"("dir" "inner.txt" "a.txt" "b.txt" "z.txt")"#);

    // ⌘B puts the tree away and back; put away while it has the keyboard,
    // it hands the keyboard to the editor.
    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(!shown("tree", cx));
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "er\n");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(shown("tree", cx));
}

#[gpui::test]
fn the_file_tree_shows_what_git_says(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("dir/inner.txt", "inner\n")]);
    project.git_init();
    project.git(&["add", "-A"]);
    project.git(&["commit", "-q", "-m", "first"]);
    fs::write(project.path("dir/inner.txt"), "changed\n").unwrap();
    fs::write(project.path("new.txt"), "new\n").unwrap();

    let (_ws, cx) = open_with(&[TREE], &project, &[], cx);
    let mark = |name: &str, cx: &mut VisualTestContext| {
        let path = project.path(name).display().to_string();
        lisp(&format!("(get tree-marks {path:?})"), cx)
    };
    assert!(wait_for(cx, |cx| lisp("tree-marks", cx) != "nil"));
    // A changed file has git's letter, and the directory it is in an M.
    assert_eq!(mark("dir/inner.txt", cx), "\"M\"");
    assert_eq!(mark("dir", cx), "\"M\"");
    assert_eq!(mark("new.txt", cx), "\"?\"");
    assert_eq!(mark("a.txt", cx), "nil");
}

#[gpui::test]
fn the_file_tree_is_a_panel_like_any_other(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let (ws, cx) = open_with(&[TREE], &project, &[], cx);
    lisp("(show-panel 'tree)", cx);
    cx.run_until_parked();
    // `y` from the top of the tree, which is under the title bar.
    let click = |y: f32, cx: &mut VisualTestContext| {
        cx.simulate_click(point(px(40.), px(TITLE + y)), Modifiers::none());
        cx.run_until_parked();
    };
    let message = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    // Set a layout as the init file would: with no workspace behind it.
    let set_layout = |source: &str, cx: &mut VisualTestContext| {
        let result = cx.update(|_, cx| crate::script::eval(source, cx));
        cx.run_until_parked();
        result
            .map(|value| value.to_string())
            .map_err(|error| error.to_string())
    };

    // Its own layout: a 32px header, then 24px rows that open on a click.
    // The second row is b.txt.
    click(32. + 24. + 12., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    assert_eq!(lisp("tree-selected", cx), "1");

    // Another layout for it: a button where the header was, taller rows,
    // drawn from the same values the program keeps.
    let layout = r#"
        (set-layout 'tree
          (fn (panel)
            (col :width 'full :height 'full
              (text (str "Close " (path-name (get panel :root)))
                    :height 40 :on-click "workspace.close_tab")
              (rows tree-entries :grow true
                (fn (entry index)
                  (row :height 30 :width 'full
                       :bg (when (= index tree-selected) 'list-active)
                       :on-click (fn ()
                                   (tree-select index)
                                   (run "tree.open")
                                   (message "opened " (get entry :name)))
                    (when (get entry :dir) (text "/"))
                    (text (get entry :name))))))))"#;
    assert_eq!(set_layout(layout, cx), Ok("nil".to_string()));

    // Rows are now 30px under a 40px button: the first is a.txt. Its
    // click is a function, which runs a command and then says so, the
    // row's own values still at hand.
    click(40. + 15., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    assert_eq!(message(cx), "opened a.txt");
    click(40. + 60. + 15., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("c.txt")));
    assert_eq!(message(cx), "opened c.txt");
    // The button runs a command by its name.
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs().len()), 3);
    click(20., cx);
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs().len()), 2);

    // A layout that fails is drawn as its error, row by row or whole; one
    // that tries to do more than describe is refused; one that never ends
    // is cut short. None of it takes the editor down, and the next layout
    // works.
    for broken in [
        "(set-layout 'tree (fn (panel) (col (rows tree-entries (fn (entry) (car 5))))))",
        "(set-layout 'tree (fn (panel) (nope)))",
        "(set-layout 'tree (fn (panel) 5))",
        "(set-layout 'tree (fn (panel) (message \"drawn\") (col)))",
        "(set-layout 'tree (fn (panel) (while true)))",
    ] {
        assert_eq!(set_layout(broken, cx), Ok("nil".to_string()), "{broken}");
        click(40. + 15., cx);
    }
    assert_eq!(cx.update(|_, cx| crate::script::take_message(cx)), None);
    assert_eq!(set_layout(layout, cx), Ok("nil".to_string()));
    click(40. + 30. + 15., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));

    // What is not a panel, or not a function, is not a layout.
    assert_eq!(
        set_layout("(set-layout 'git (fn (panel) (col)))", cx),
        Err("1:1: git is not a panel (tree)".to_string())
    );
    assert_eq!(
        set_layout("(set-layout 'tree 5)", cx),
        Err("1:1: set-layout: expected a function, got 5".to_string())
    );

    // Running the init file again forgets the panels: an editor with no
    // init file has none. A click where the tree was lands in the editor.
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.mlisp"), cx));
    assert!(!ran);
    cx.run_until_parked();
    assert_eq!(
        set_layout("(set-layout 'tree (fn (panel) (col)))", cx),
        Err("1:1: tree is not a panel ()".to_string())
    );
}

#[gpui::test]
fn scripts_change_keys_and_options(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one two\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open_with(&[TREE], &project, &["a.txt", "b.txt"], cx);
    let message = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    // Type `:eval <source>` as a user would.
    let eval = |source: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(":");
        cx.simulate_input(&format!("eval {source}"));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    // The command line shows what a form came to.
    assert_eq!(eval("(+ 1 2)", cx), "3");
    assert_eq!(eval("(defn twice (n) (* 2 n))", cx), "twice");
    // The interpreter is one for the session: twice is still there.
    assert_eq!(eval("(twice 21)", cx), "42");

    // An editor key, in two modes at once.
    assert_eq!(
        eval(
            r#"(bind '(normal visual) "<Space>n" "workspace.next_tab")"#,
            cx
        ),
        "nil"
    );
    cx.simulate_keystrokes("space n");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.simulate_keystrokes("space n");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    // An engine command, keys for keys, and a key taken away.
    eval(r#"(bind :normal "Q" "operator.delete")"#, cx);
    cx.simulate_keystrokes("shift-q w");
    assert_eq!(text(&ws, cx), "two\n");
    eval(r#"(bind-keys "normal" "Z" "dw")"#, cx);
    cx.simulate_keystrokes("shift-z");
    assert_eq!(text(&ws, cx), "\n");
    assert_eq!(eval(r#"(unbind 'normal "gt")"#, cx), "true");
    assert_eq!(eval(r#"(unbind 'normal "gt")"#, cx), "false");
    cx.simulate_keystrokes("g t");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    // A panel's key.
    eval(r#"(bind 'tree "x" "tree.close")"#, cx);
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    cx.simulate_keystrokes("i h i escape");
    assert_eq!(text(&ws, cx), "hi\n");

    // A shortcut of the window's, which works wherever the keyboard is,
    // and stops working when it is taken away.
    assert_eq!(
        eval(r#"(bind 'app "cmd-shift-j" "workspace.next_tab")"#, cx),
        "nil"
    );
    cx.simulate_keystrokes("cmd-shift-j");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    assert_eq!(eval(r#"(unbind 'app "cmd-shift-j")"#, cx), "true");
    assert_eq!(eval(r#"(unbind 'app "cmd-shift-j")"#, cx), "false");
    cx.simulate_keystrokes("cmd-shift-j");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    // Options, and something to say.
    eval("(set-option 'font-size 18.5)", cx);
    eval(r#"(set-option 'font-family "Menlo")"#, cx);
    cx.update(|_, cx| {
        let settings = cx.global::<crate::Settings>();
        assert_eq!(settings.font_size, 18.5);
        assert_eq!(settings.font_family.as_deref(), Some("Menlo"));
    });
    assert_eq!(
        eval(r#"(do (message "size " 18.5) 'ignored)"#, cx),
        "size 18.5"
    );

    // Mistakes are explained, and change nothing.
    assert_eq!(
        eval(r#"(bind 'normal "X" "no.such_command")"#, cx),
        "1:1: there is no command no.such_command"
    );
    assert_eq!(
        eval(r#"(bind 'nomal "X" "tree.close")"#, cx),
        "1:1: nomal is neither a mode (normal visual operator insert), app, nor a panel (tree)"
    );
    assert_eq!(
        eval(r#"(bind 'app "cmd-nope-j" "tree.close")"#, cx),
        "1:1: \"cmd-nope-j\" is no shortcut: in app they are written like cmd-shift-p"
    );
    assert_eq!(
        eval(r#"(bind-keys 'tree "X" "j")"#, cx),
        "1:1: keys stand for other keys only in an editor mode, not in tree"
    );
    assert_eq!(
        eval("(set-option 'font-sise 12)", cx),
        "1:1: font-sise is not an option (font-size font-family lsp key-timeout auto-pairs format-on-save tab-width relative-number scroll-off cursor-line color-column wrap inlay-hints indent-guides sticky-scroll undo-file theme project-env)"
    );
    assert_eq!(
        eval("(set-option 'font-size 0)", cx),
        "1:1: 0 is no size for a font"
    );
    assert_eq!(eval("(car", cx), "1:1: this ( is never closed");
    // A program that never ends is stopped; the editor carries on.
    assert_eq!(
        eval("(while true)", cx),
        "1:1: stopped after 10000000 steps"
    );
    assert_eq!(eval("(twice 2)", cx), "4");
    assert_eq!(message(cx), "4");
}

/// The real window: the workspace inside gpui's root, whose own keys
/// (`tab` moves focus) come before the views'.
#[gpui::test]
fn tab_reaches_the_editor_in_a_real_window(cx: &mut TestAppContext) {
    use gpui::AppContext as _;
    let project = Project::new(&[("a.rs", "fn main() {\n}\n")]);
    cx.update(|cx| {
        crate::init(cx);
        cx.global_mut::<crate::Settings>().lsp = false;
        bind_example_keys(cx);
    });
    let root = project.0.clone();
    let files = vec![project.path("a.rs")];
    let mut workspace = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(root, files, window, cx));
        workspace = Some(ws.clone());
        gpui::base::Root::new(ws, window, cx)
    });
    let ws = workspace.unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes("j o tab x escape");
    assert_eq!(text(&ws, cx), "fn main() {\n}\n    x\n");
}

#[gpui::test]
fn the_tab_width_option_is_every_files(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.mlisp", "(a\n  b)\n")]);
    let (ws, cx) = open(&project, &["a.mlisp"], cx);

    // By the language, and the file's own indentation: 2.
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "(a\n  x\n  b)\n");

    // The option, in a file already open: `Tab` and `>>` both.
    assert_eq!(lisp("(set-option 'tab-width 4)", cx), "nil");
    cx.simulate_keystrokes("u o tab x escape");
    assert_eq!(text(&ws, cx), "(a\n    x\n  b)\n");
    cx.simulate_keystrokes("u g g > >");
    assert_eq!(text(&ws, cx), "    (a\n  b)\n");

    // nil goes back to the file's own.
    assert_eq!(lisp("(set-option 'tab-width nil)", cx), "nil");
    cx.simulate_keystrokes("u > >");
    assert_eq!(text(&ws, cx), "  (a\n  b)\n");

    for bad in ["0", "17", "\"4\""] {
        let source = format!("(set-option 'tab-width {bad})");
        assert!(
            cx.update(|_, cx| crate::script::eval(&source, cx)).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        cx.update(|_, cx| cx.global::<crate::Settings>().tab_width),
        None
    );
}

#[test]
fn relative_line_numbers_count_from_the_cursor() {
    use crate::editor::line_number;
    // Off: every line its own number.
    assert_eq!(line_number(0, 4, false, 3), "  1");
    assert_eq!(line_number(4, 4, false, 3), "  5");
    // On: the distance, either way; the cursor's line keeps its number.
    assert_eq!(line_number(0, 4, true, 3), "  4");
    assert_eq!(line_number(4, 4, true, 3), "  5");
    assert_eq!(line_number(9, 4, true, 3), "  5");
    assert_eq!(line_number(1203, 0, true, 4), "1203");
}

#[gpui::test]
fn the_relative_number_option(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n")]);
    let (_ws, cx) = open(&project, &["a.txt"], cx);
    let relative = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| cx.global::<crate::Settings>().relative_number)
    };
    assert!(!relative(cx));
    assert_eq!(lisp("(set-option 'relative-number true)", cx), "nil");
    assert!(relative(cx));
    assert_eq!(lisp("(set-option 'relative-number nil)", cx), "nil");
    assert!(!relative(cx));
    let bad = "(set-option 'relative-number 1)";
    assert!(cx.update(|_, cx| crate::script::eval(bad, cx)).is_err());
}

#[gpui::test]
fn a_tab_has_options_of_its_own(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.mlisp", "(a\n  b)\n"), ("b.mlisp", "(b\n  c)\n")]);
    let (ws, cx) = open(&project, &["a.mlisp"], cx);
    assert_eq!(eval_in(&ws, "(buffer-language)", cx), r#""mlisp""#);

    // This tab's width is 4, over the language's 2 and over every tab's 8.
    assert_eq!(lisp("(set-option 'tab-width 8)", cx), "nil");
    assert_eq!(eval_in(&ws, "(set-buffer-option 'tab-width 4)", cx), "nil");
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "(a\n    x\n  b)\n");
    // Brackets close as they are typed, but not in this tab.
    assert_eq!(
        eval_in(&ws, "(set-buffer-option 'auto-pairs false)", cx),
        "nil"
    );
    cx.simulate_keystrokes("u o ( escape");
    assert_eq!(text(&ws, cx), "(a\n(\n  b)\n");
    cx.simulate_keystrokes("u");

    // Another tab goes by every tab's.
    cx.simulate_keystrokes(": e space b . m l i s p enter");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.mlisp")));
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "(b\n        x\n  c)\n");
    cx.simulate_keystrokes("u o ( escape");
    assert_eq!(text(&ws, cx), "(b\n()\n  c)\n");

    // The first keeps its own, until they are reset.
    cx.simulate_keystrokes(": e space a . m l i s p enter");
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "(a\n    x\n  b)\n");
    assert_eq!(eval_in(&ws, "(reset-buffer-option 'tab-width)", cx), "nil");
    assert_eq!(eval_in(&ws, "(reset-buffer-option 'auto-pairs)", cx), "nil");
    cx.simulate_keystrokes("u o tab ( escape");
    assert_eq!(text(&ws, cx), "(a\n        ()\n  b)\n");

    // The ones shown as it is drawn, and read as it is saved.
    let options = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            (editor.relative_number(cx), editor.format_on_save(cx))
        })
    };
    assert_eq!(lisp("(set-option 'relative-number true)", cx), "nil");
    assert_eq!(options(&ws, cx), (true, false));
    eval_in(&ws, "(set-buffer-option 'relative-number false)", cx);
    eval_in(&ws, "(set-buffer-option 'format-on-save true)", cx);
    assert_eq!(options(&ws, cx), (false, true));
    cx.simulate_keystrokes(": e space b . m l i s p enter");
    assert_eq!(options(&ws, cx), (true, false));

    // Only the options a file can have its own of, with values they take.
    for (bad, said) in [
        (
            "(set-buffer-option 'font-size 16)",
            "not an option a tab has",
        ),
        ("(reset-buffer-option 'lsp)", "not an option a tab has"),
        ("(set-buffer-option 'tab-width 0)", "no width for a tab"),
        ("(set-buffer-option 'tab-width nil)", "expected"),
        (
            "(set-buffer-option 'relative-number 1)",
            "expected true or false",
        ),
    ] {
        let message = eval_in(&ws, bad, cx);
        assert!(message.contains(said), "{bad}: {message}");
    }
    // A program with no workspace, as the init file, has no tab to change.
    let none = cx.update(|_, cx| crate::script::eval("(set-buffer-option 'tab-width 4)", cx));
    assert!(none.unwrap_err().to_string().contains("there is no tab up"));
}

#[gpui::test]
fn the_init_file_gives_options_by_language(cx: &mut TestAppContext) {
    // As the docs have it: a hook of the init file's, told of every file
    // that comes up, the first one too.
    let project = Project::new(&[("a.mlisp", "(a\n  b)\n"), ("b.txt", "bee\n")]);
    let init = r#"
        (def languages nil)
        (add-hook 'buffer-entered
          (fn (path)
            (set! languages (cons (buffer-language) languages))
            (when (= (buffer-language) "mlisp")
              (set-buffer-option 'tab-width 3))))"#;
    let (ws, cx) = open_with(&[init], &project, &["a.mlisp"], cx);
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "(a\n   x\n  b)\n");

    // A file with no language is "text", as the status bar has it, and
    // keeps every tab's width: 4 here, as the file is indented by nothing.
    cx.simulate_keystrokes(": e space b . t x t enter");
    assert_eq!(lisp("languages", cx), r#"("text" "mlisp")"#);
    assert_eq!(message(&ws, cx), "");
    cx.simulate_keystrokes("o tab x escape");
    assert_eq!(text(&ws, cx), "bee\n    x\n");
}

#[gpui::test]
fn the_view_keeps_lines_about_the_cursor(cx: &mut TestAppContext) {
    let text: String = (0..200).map(|n| format!("{n}\n")).collect();
    let project = Project::new(&[("a.txt", text.as_str())]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let view = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            let line = editor.buf.line_of(editor.vim.cursor);
            let (top, visible) = editor.view();
            (top, visible, line)
        })
    };
    let (_, visible, _) = view(&ws, cx);
    assert!(visible >= 12, "{visible} lines on screen");
    assert_eq!(lisp("(set-option 'scroll-off 5)", cx), "nil");

    // Down to the last line on screen: the view goes on, 5 lines past it.
    let keys = |count: usize, key: &str| {
        let digits: Vec<String> = count.to_string().chars().map(String::from).collect();
        format!("{} {key}", digits.join(" "))
    };
    cx.simulate_keystrokes(&keys(visible - 1, "j"));
    assert_eq!(view(&ws, cx), (5, visible, visible - 1));

    // `ctrl-e` takes the cursor along 5 lines below the top, and `L` stops
    // 5 above the bottom: neither has the view scroll back.
    cx.simulate_keystrokes("g g ctrl-e");
    assert_eq!(view(&ws, cx), (1, visible, 6));
    cx.simulate_keystrokes("shift-l");
    assert_eq!(view(&ws, cx), (1, visible, visible - 5));
    cx.simulate_keystrokes("shift-h");
    assert_eq!(view(&ws, cx), (1, visible, 6));

    // This tab's own: none at all.
    eval_in(&ws, "(set-buffer-option 'scroll-off 0)", cx);
    cx.simulate_keystrokes("shift-l");
    assert_eq!(view(&ws, cx), (1, visible, visible));
    eval_in(&ws, "(reset-buffer-option 'scroll-off)", cx);
    cx.simulate_keystrokes("shift-l");
    assert_eq!(view(&ws, cx), (1, visible, visible - 5));

    for bad in ["-1", "1000", "true"] {
        let source = format!("(set-option 'scroll-off {bad})");
        assert!(
            cx.update(|_, cx| crate::script::eval(&source, cx)).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn lines_wrap_where_a_char_no_longer_fits() {
    use crate::editor::wrap_breaks;
    assert_eq!(wrap_breaks("", 4, 5), [0]);
    assert_eq!(wrap_breaks("abcd", 4, 5), [0]);
    // A row filled: the place after the end is on one more.
    assert_eq!(wrap_breaks("abcde", 4, 5), [0, 5]);
    assert_eq!(wrap_breaks("abcdef", 4, 5), [0, 5]);
    assert_eq!(wrap_breaks("abcdefghijk", 4, 5), [0, 5, 10]);
    // A tab to its stop, from the start of the line: 4 cells, then `a`.
    assert_eq!(wrap_breaks("\tab", 4, 5), [0, 2]);
    // A wide char that does not fit goes on whole; one wider than a row
    // has a row of its own.
    assert_eq!(wrap_breaks("ab中", 4, 3), [0, 2]);
    assert_eq!(wrap_breaks("中", 4, 1), [0, 1]);
}

#[test]
fn indent_guides_go_by_the_lines_indents() {
    use crate::editor::{block_guide, indent_cells, line_indents};
    assert_eq!(indent_cells("    x", 4), Some(4));
    assert_eq!(indent_cells("\t\tx", 4), Some(8));
    assert_eq!(indent_cells("  \tx", 4), Some(4));
    assert_eq!(indent_cells("x", 4), Some(0));
    assert_eq!(indent_cells("   ", 4), None);
    assert_eq!(indent_cells("", 4), None);

    // A blank line takes the less of the lines either side; one at the
    // start or end of what is looked at, the margin.
    let raw = [
        None,
        Some(0),
        Some(4),
        None,
        Some(8),
        None,
        Some(4),
        Some(0),
        None,
    ];
    assert_eq!(line_indents(&raw), [0, 0, 4, 4, 8, 4, 4, 0, 0]);

    //   0 fn f() {
    //   1     if x {
    //   2         y();
    //   3
    //   4         z();
    //   5     }
    //   6 }
    let indents = line_indents(&[Some(0), Some(4), Some(8), None, Some(8), Some(4), Some(0)]);
    // In the `if`'s block, blank line and all: the guide at column 4.
    assert_eq!(block_guide(&indents, 2, 4), Some((4, 2, 4)));
    assert_eq!(block_guide(&indents, 3, 4), Some((4, 2, 4)));
    // On the `if` line: the function's block, at column 0.
    assert_eq!(block_guide(&indents, 1, 4), Some((0, 1, 5)));
    // At the margin: none.
    assert_eq!(block_guide(&indents, 0, 4), None);
    // An indent between steps belongs to the step below it.
    assert_eq!(block_guide(&[0, 6, 6, 0], 1, 4), Some((4, 1, 2)));
}

#[test]
fn color_columns_are_a_cell_wide_down_the_text() {
    use crate::editor::column_bands;
    use gpui::{Bounds, point, size};
    let text = Bounds::new(point(px(40.), px(0.)), size(px(800.), px(600.)));
    let cell = px(10.);
    let lefts = |columns: &[usize], scroll: f32| -> Vec<f32> {
        column_bands(columns, text, cell, px(scroll))
            .iter()
            .map(|band| {
                assert_eq!(
                    (band.size.width, band.top(), band.size.height),
                    (cell, px(0.), px(600.))
                );
                f32::from(band.left())
            })
            .collect()
    };
    // Column 1 is the first cell; 80 the 80th.
    assert_eq!(lefts(&[1, 80], 0.), [40., 830.]);
    // Scrolled sideways, they go with the text, and off it are not drawn.
    assert_eq!(lefts(&[1, 80], 100.), [730.]);
    assert_eq!(lefts(&[200], 0.), Vec::<f32>::new());
}

#[gpui::test]
fn the_cursor_line_and_color_column_options(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let options = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            (editor.cursor_line(cx), editor.color_columns(cx))
        })
    };
    // The line marked, no column: as before there were options.
    assert_eq!(options(&ws, cx), (true, vec![]));
    assert_eq!(lisp("(set-option 'cursor-line false)", cx), "nil");
    assert_eq!(lisp("(set-option 'color-column 80)", cx), "nil");
    assert_eq!(options(&ws, cx), (false, vec![80]));
    assert_eq!(lisp("(set-option 'color-column '(80 120))", cx), "nil");
    assert_eq!(options(&ws, cx), (false, vec![80, 120]));

    // This tab's own; nil is none of its own, until it is reset.
    eval_in(&ws, "(set-buffer-option 'cursor-line true)", cx);
    eval_in(&ws, "(set-buffer-option 'color-column nil)", cx);
    assert_eq!(options(&ws, cx), (true, vec![]));
    cx.simulate_keystrokes(": e space b . t x t enter");
    assert_eq!(options(&ws, cx), (false, vec![80, 120]));
    cx.simulate_keystrokes(": e space a . t x t enter");
    eval_in(&ws, "(reset-buffer-option 'color-column)", cx);
    assert_eq!(options(&ws, cx), (true, vec![80, 120]));

    for bad in ["0", "1000", "'(80 \"x\")", "true"] {
        let source = format!("(set-option 'color-column {bad})");
        assert!(
            cx.update(|_, cx| crate::script::eval(&source, cx)).is_err(),
            "{bad}"
        );
    }
}

#[gpui::test]
fn undo_goes_on_where_the_file_was_left(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("crlf.txt", "x\r\ny\r\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let undo = project.path(".state/undo");
    cx.update(|_, cx| cx.global_mut::<crate::Settings>().undo_dir = Some(undo.clone()));
    let kept = |name: &str| crate::undo_file::path_for(&undo, &project.path(name));
    // Closed, whatever was not saved, and opened again.
    let reopen = |name: &str, cx: &mut VisualTestContext| {
        cx.simulate_input(":bd!");
        cx.simulate_keystrokes("enter");
        cx.simulate_input(&format!(":e {name}"));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };

    // Off, as at first: nothing is kept.
    cx.simulate_keystrokes("A space t w o escape : w enter");
    assert!(!kept("a.txt").exists());
    reopen("a.txt", cx);
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two\n");

    // On: two saves, and the file closed and opened again.
    assert_eq!(lisp("(set-option 'undo-file true)", cx), "nil");
    cx.simulate_keystrokes("A space t h r e e escape : w enter");
    cx.simulate_keystrokes("A space f o u r escape : w enter");
    assert!(kept("a.txt").exists());
    reopen("a.txt", cx);
    assert_eq!(text(&ws, cx), "one two three four\n");
    assert!(!dirty(&ws, cx));
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two three\n");
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two\n");
    // What came before the option was on is not there.
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two\n");
    cx.simulate_keystrokes("ctrl-r ctrl-r");
    assert_eq!(text(&ws, cx), "one two three four\n");
    assert!(!dirty(&ws, cx));

    // Saved in the middle of an insert: that much of it is a step.
    assert_eq!(lisp(r#"(bind 'app "cmd-s" "workspace.save")"#, cx), "nil");
    cx.simulate_keystrokes("A space f i v e cmd-s space s i x escape");
    let disk = fs::read_to_string(project.path("a.txt")).unwrap();
    assert_eq!(disk, "one two three four five\n");
    // What came after is not saved, and goes.
    reopen("a.txt", cx);
    assert_eq!(text(&ws, cx), "one two three four five\n");
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two three four\n");

    // Changed elsewhere since: what was kept is of another text.
    fs::write(project.path("a.txt"), "changed\n").unwrap();
    reopen("a.txt", cx);
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "changed\n");

    // Line endings as the file has them, the history as the buffer does.
    reopen("crlf.txt", cx);
    cx.simulate_keystrokes("A z escape : w enter");
    assert_eq!(
        fs::read_to_string(project.path("crlf.txt")).unwrap(),
        "xz\r\ny\r\n"
    );
    reopen("crlf.txt", cx);
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "x\ny\n");

    // Nonsense where a history should be: the file opens as any other.
    fs::write(kept("crlf.txt"), "{ not json").unwrap();
    reopen("crlf.txt", cx);
    assert_eq!(text(&ws, cx), "xz\ny\n");
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "xz\ny\n");
    assert!(!dirty(&ws, cx));
}

#[gpui::test]
fn wrapped_lines_scroll_and_click_by_rows(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    let view = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.wrapped_view());
    assert_eq!(lisp("(set-option 'wrap true)", cx), "nil");
    cx.simulate_keystrokes("l h");
    let (Some(cells), _, _) = view(cx) else {
        panic!("not wrapped");
    };
    let (_, visible) = editor.read_with(cx, |e, _| e.view());
    assert!(cells > 10 && visible > 10, "{cells} cells, {visible} rows");

    // A line taller than the view, then 40 lines of 3 rows each.
    let tall: String = "t".repeat(cells * (visible + 5));
    let mut text = format!("{tall}\n");
    for n in 0..40 {
        let c = (b'a' + n % 26) as char;
        text += &format!("{}\n", c.to_string().repeat(cells * 5 / 2));
    }
    editor.update(cx, |e, cx| e.replace_text(&text, cx));

    // `gj` and `gk` by rows of the view; `j` by lines.
    cx.simulate_keystrokes("g g 0 l g j");
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), cells + 1);
    cx.simulate_keystrokes("g k");
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), 1);
    cx.simulate_keystrokes("j");
    let second = editor.read_with(cx, |e, _| e.buf.line_start(1) + 1);
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), second);

    // The end of the tall line is on screen, its start not.
    cx.simulate_keystrokes("g g $");
    let (_, top, rows) = view(cx);
    assert!(top.0 == 0 && top.1 > 0, "{top:?}");
    assert!(rows.is_some(), "the cursor is off screen");

    // Down the 3-row lines, 2 rows kept below the cursor all the way.
    assert_eq!(lisp("(set-option 'scroll-off 2)", cx), "nil");
    for _ in 0..20 {
        cx.simulate_keystrokes("j");
        let (_, _, rows) = view(cx);
        let rows = rows.expect("the cursor is off screen");
        assert!(rows + 2 < visible, "{rows} rows down of {visible}");
    }

    // `ctrl-e` goes a row down, and stays there: the cursor goes with it.
    cx.simulate_keystrokes("g g");
    let (_, top, _) = view(cx);
    assert_eq!(top, (0, 0));
    cx.simulate_keystrokes("ctrl-e ctrl-e");
    let (_, top, rows) = view(cx);
    assert_eq!(top, (0, 2));
    assert!(rows.is_some_and(|rows| rows >= 2), "{rows:?}");

    // `H` and `L` stop short of the edges by the margin, and do not move
    // the view; flash and paging see every line wholly on screen.
    cx.simulate_keystrokes("2 0 G");
    let (_, top, _) = view(cx);
    assert!(top.1 > 0, "a line cut at the top: {top:?}");
    let viewport = editor.read_with(cx, |e, cx| e.viewport(cx));
    assert_eq!((viewport.top, viewport.scroll_off), (top.0 + 1, 2));
    cx.simulate_keystrokes("shift-h");
    let (_, after, rows) = view(cx);
    assert_eq!(after, top);
    assert!(rows.is_some_and(|rows| rows >= 2), "{rows:?}");
    cx.simulate_keystrokes("shift-l");
    let (_, after, rows) = view(cx);
    assert_eq!(after, top);
    assert!(rows.is_some_and(|rows| rows + 2 < visible), "{rows:?}");

    // A click on a line's second row is the char painted there.
    cx.simulate_keystrokes("G");
    let target = editor.read_with(cx, |e, _| e.buf.line_start(39) + cells + 3);
    let cell = editor
        .read_with(cx, |e, _| e.cell_origin(target))
        .expect("on screen");
    cx.simulate_click(point(cell.x + px(2.), cell.y + px(4.)), Modifiers::none());
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), target);

    // A tab of its own: this one does not wrap, another does.
    eval_in(&ws, "(set-buffer-option 'wrap false)", cx);
    cx.simulate_keystrokes("l h");
    assert_eq!(view(cx).0, None);
    cx.simulate_keystrokes(": e space b . t x t enter");
    cx.simulate_keystrokes("l h");
    let other = ws.read_with(cx, |ws, cx| {
        ws.active_editor().unwrap().read(cx).wrapped_view()
    });
    assert_eq!(other.0, Some(cells));
}

#[gpui::test]
fn the_theme_option_picks_colors(cx: &mut TestAppContext) {
    use gpui::component::{ActiveTheme as _, Theme};
    let project = Project::new(&[("a.txt", "a\n")]);
    let (_ws, cx) = open(&project, &["a.txt"], cx);
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/themes");
    cx.update(|_, cx| crate::theme::load_dir(&examples, cx));
    // A color as `#rrggbb`.
    let hex = |color: gpui::Hsla| {
        let rgba = gpui::Rgba::from(color);
        let byte = |v: f32| (v * 255.).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(rgba.r),
            byte(rgba.g),
            byte(rgba.b)
        )
    };
    let colors = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let theme = cx.theme();
            let keyword = theme.highlight_theme.style("keyword").and_then(|s| s.color);
            (
                theme.mode.is_dark(),
                hex(theme.background),
                keyword.map(hex),
            )
        })
    };
    let system_is_dark = cx.update(|window, _| {
        matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        )
    });

    // One theme: its colors, the editor's and the syntax's, exactly.
    assert_eq!(lisp("(set-option 'theme \"Catppuccin Mocha\")", cx), "nil");
    let mocha = (true, "#1e1e2e".to_string(), Some("#cba6f7".to_string()));
    assert_eq!(colors(cx), mocha);
    // And it stays, whatever the system's appearance comes to.
    cx.update(|window, cx| crate::theme::apply(Some(window), cx));
    assert_eq!(colors(cx), mocha);

    // A light one and a dark one: as the system is.
    let pair = r#"(set-option 'theme '("Catppuccin Latte" "Catppuccin Mocha"))"#;
    assert_eq!(lisp(pair, cx), "nil");
    let latte = (false, "#eff1f5".to_string(), Some("#8839ef".to_string()));
    assert_eq!(
        colors(cx),
        if system_is_dark { mocha.clone() } else { latte }
    );

    // None: gpui-component's own, as the system is.
    assert_eq!(lisp("(set-option 'theme nil)", cx), "nil");
    let (dark, background, _) = colors(cx);
    assert_eq!(dark, system_is_dark);
    assert!(
        background != "#1e1e2e" && background != "#eff1f5",
        "{background}"
    );
    let name = cx.update(|_, cx| Theme::global(cx).theme_name().clone());
    assert!(name.starts_with("Default"), "{name}");

    // Names that are not there, or a pair the wrong way round.
    for (bad, said) in [
        (
            "(set-option 'theme \"Mocha\")",
            "there is no theme \"Mocha\" (Catppuccin Latte, Catppuccin Mocha",
        ),
        (
            r#"(set-option 'theme '("Catppuccin Mocha" "Catppuccin Latte"))"#,
            "is a dark theme, not a light one",
        ),
        (
            r#"(set-option 'theme '("Catppuccin Latte"))"#,
            "a light one's and a dark one's",
        ),
    ] {
        let error = cx.update(|_, cx| crate::script::eval(bad, cx)).unwrap_err();
        assert!(error.to_string().contains(said), "{bad}: {error}");
    }
    assert_eq!(colors(cx).1, background);
}

#[gpui::test]
fn inlay_hints_stand_in_the_line(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "let x = 1;\na\tb\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    // `: i32` after `x`, `XY` before the tab of the second line.
    let tab_line = editor.read_with(cx, |e, _| e.buf.line_start(1));
    editor.update(cx, |e, _| {
        e.set_inlay_hints(&[(5, ": i32"), (tab_line + 1, "XY")])
    });
    let x = |offset: usize, cx: &mut VisualTestContext| {
        let origin: f32 = editor.read_with(cx, |e, _| {
            let line = e.buf.line_start(e.buf.line_of(offset));
            (e.cell_origin(offset).unwrap().x - e.cell_origin(line).unwrap().x).into()
        });
        let cell: f32 = editor.read_with(cx, |e, _| {
            (e.cell_origin(1).unwrap().x - e.cell_origin(0).unwrap().x).into()
        });
        (origin / cell).round() as usize
    };
    let redraw = |cx: &mut VisualTestContext| cx.simulate_keystrokes("l h");

    // Off, as at first: not there.
    redraw(cx);
    assert_eq!(x(6, cx), 6);

    // On: the `=` after the hint is 5 cells further, on screen and to a
    // click; a tab stops where it would without the hint, and what is
    // after it is the hint's 2 cells further.
    assert_eq!(lisp("(set-option 'inlay-hints true)", cx), "nil");
    redraw(cx);
    assert_eq!(x(6, cx), 11);
    assert_eq!(x(tab_line + 2, cx), 6);
    let cell = editor.read_with(cx, |e, _| e.cell_origin(6)).unwrap();
    cx.simulate_click(point(cell.x + px(2.), cell.y + px(4.)), Modifiers::none());
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), 6);

    // A tab of its own, and none where lines wrap.
    eval_in(&ws, "(set-buffer-option 'inlay-hints false)", cx);
    redraw(cx);
    assert_eq!(x(6, cx), 6);
    eval_in(&ws, "(reset-buffer-option 'inlay-hints)", cx);
    eval_in(&ws, "(set-buffer-option 'wrap true)", cx);
    redraw(cx);
    assert_eq!(x(6, cx), 6);
    eval_in(&ws, "(reset-buffer-option 'wrap)", cx);
    redraw(cx);
    assert_eq!(x(6, cx), 11);

    // Hints go with the text as it is edited, and an answer for the text
    // as it was before an edit is not taken.
    let version = editor.read_with(cx, |e, _| e.buf.version());
    cx.simulate_keystrokes("g g I l e t space escape");
    assert_eq!(text(&ws, cx), "let let x = 1;\na\tb\n");
    assert_eq!(x(10, cx), 15);
    let stale = lsp_types::InlayHint {
        position: lsp_types::Position::new(0, 1),
        label: lsp_types::InlayHintLabel::String("stale".into()),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: None,
        data: None,
    };
    let taken = editor.update(cx, |e, _| {
        e.apply_inlay_hints(version, std::slice::from_ref(&stale))
    });
    assert!(!taken);
    assert_eq!(
        editor.read_with(cx, |e, _| e.inlay_hint_texts()),
        [": i32", "XY"]
    );
    let now = editor.read_with(cx, |e, _| e.buf.version());
    let taken = editor.update(cx, |e, _| e.apply_inlay_hints(now, &[stale]));
    assert!(taken);
    assert_eq!(editor.read_with(cx, |e, _| e.inlay_hint_texts()), ["stale"]);
}

#[gpui::test]
fn the_indent_guides_option(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let on = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            ws.active_editor().unwrap().read(cx).indent_guides(cx)
        })
    };
    assert!(!on(&ws, cx));
    assert_eq!(lisp("(set-option 'indent-guides true)", cx), "nil");
    assert!(on(&ws, cx));
    eval_in(&ws, "(set-buffer-option 'indent-guides false)", cx);
    assert!(!on(&ws, cx));
    cx.simulate_keystrokes(": e space b . t x t enter");
    assert!(on(&ws, cx));
}

#[test]
fn sticky_scroll_gives_each_row_the_next_scope_in() {
    use crate::editor::sticky_scopes;
    //   0 mod m {
    //   1     fn f() {
    //   2         a();
    //   3         b();
    //   4     }
    //   5     fn g() {
    //   6         c();
    //   7     }
    //   8 }
    let (m, f, g) = ((0, 8), (1, 4), (5, 7));
    let scopes = |line: usize| match line {
        1 | 5 | 8 => vec![m],
        2..=4 => vec![m, f],
        6 | 7 => vec![m, g],
        _ => vec![],
    };
    // The view from `top` on, a line to a row.
    let view = |top: usize| move |row: usize| Some((top + row, top + row)).filter(|l| l.0 < 9);
    let far = 100;
    assert_eq!(sticky_scopes(5, far, view(0), scopes), []);
    // `f`'s first line, gone under `m`'s, stands in the row after it.
    assert_eq!(sticky_scopes(5, far, view(1), scopes), [m, f]);
    assert_eq!(sticky_scopes(5, far, view(2), scopes), [m, f]);
    // `f`'s stands over its last line too; then under the second row
    // `f` has ended and `g` not begun: `m` alone.
    assert_eq!(sticky_scopes(5, far, view(3), scopes), [m, f]);
    assert_eq!(sticky_scopes(5, far, view(4), scopes), [m]);
    assert_eq!(sticky_scopes(5, far, view(6), scopes), [m, g]);
    assert_eq!(sticky_scopes(5, far, view(9), scopes), []);
    // No more than so many.
    assert_eq!(sticky_scopes(1, far, view(2), scopes), [m]);
    // Never over the cursor's line, though part of a row be over it.
    assert_eq!(sticky_scopes(5, 3, view(2), scopes), [m]);
    assert_eq!(sticky_scopes(5, 2, view(2), scopes), []);
    let part = |row: usize| Some((2 + row, 3 + row));
    assert_eq!(sticky_scopes(5, 4, part, scopes), [m]);
}

#[gpui::test]
fn sticky_scroll_shows_where_the_view_is(cx: &mut TestAppContext) {
    use gpui::{ScrollDelta, ScrollEvent};
    let mut text = String::from("fn first() {}\nmod m {\nimpl X {\n    fn f() {\n");
    for n in 0..60 {
        text += &format!("        let x{n} = {n};\n");
    }
    text += "    }\n}\n}\n";
    let project = Project::new(&[("a.rs", &text)]);
    let (ws, cx) = open(&project, &["a.rs"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    let sticky = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.sticky_lines());
    let lines = |cx: &mut VisualTestContext| -> Vec<usize> {
        sticky(cx).into_iter().map(|(line, _)| line).collect()
    };
    let (_, visible) = editor.read_with(cx, |e, _| e.view());
    assert!(visible >= 12, "{visible} rows");

    // Off, nothing stands at the top.
    cx.simulate_keystrokes("4 0 G z z l h");
    assert_eq!(lines(cx), Vec::<usize>::new());

    // On: the module, the impl and the function the view is down in.
    assert_eq!(lisp("(set-option 'sticky-scroll true)", cx), "nil");
    cx.simulate_keystrokes("l h");
    assert_eq!(lines(cx), [1, 2, 3]);

    // A click on the function's goes to it, where it stood; but for the
    // scroll-off, which is kept as ever.
    assert_eq!(lisp("(set-option 'scroll-off 0)", cx), "nil");
    let (_, bounds) = sticky(cx)[2];
    cx.simulate_click(bounds.center(), Modifiers::none());
    let at = editor.read_with(cx, |e, _| e.buf.line_start(3) + 4);
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), at);
    assert_eq!(editor.read_with(cx, |e, _| e.visible_lines().0), 1);
    // Its lines are in sight now: none stands over them.
    cx.simulate_keystrokes("l h");
    assert_eq!(lines(cx), Vec::<usize>::new());

    // As the line after a scope comes up, its line is pushed up under the
    // one before: `f` ends with line 64, here under the third row.
    cx.simulate_keystrokes("6 3 G z t 4 j");
    assert_eq!(lines(cx), [1, 2, 3]);
    let row = sticky(cx)[2].1;
    let height = row.size.height;
    cx.simulate_event(ScrollEvent {
        position: point(row.center().x, row.top() + height * 6.),
        delta: ScrollDelta::Pixels(point(px(0.), -height / 2.)),
        ..Default::default()
    });
    cx.run_until_parked();
    let pushed = sticky(cx)[2].1;
    assert_eq!(pushed.top(), row.top() - height / 2.);

    // Where lines wrap as well.
    assert_eq!(lisp("(set-option 'wrap true)", cx), "nil");
    cx.simulate_keystrokes("4 0 G z z l h");
    assert_eq!(lines(cx), [1, 2, 3]);
    let (_, bounds) = sticky(cx)[2];
    cx.simulate_click(bounds.center(), Modifiers::none());
    assert_eq!(editor.read_with(cx, |e, _| e.vim.cursor), at);
    assert_eq!(editor.read_with(cx, |e, _| e.visible_lines().0), 1);

    // A tab can have its own.
    eval_in(&ws, "(set-buffer-option 'sticky-scroll false)", cx);
    cx.simulate_keystrokes("4 0 G z z l h");
    assert_eq!(lines(cx), Vec::<usize>::new());
}

#[gpui::test]
fn scripts_define_commands(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let (ws, cx) = open(&project, &["a.txt", "b.txt", "c.txt"], cx);
    let message = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    // Type a command line as a user would, and read the status bar. The
    // line is one line: what is broken up below for reading is joined.
    let ex = |line: &str, cx: &mut VisualTestContext| {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        cx.simulate_keystrokes(":");
        cx.simulate_input(&line);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    let path = |name: &str| project.path(name).display().to_string();

    // A command made of other commands...
    assert_eq!(
        ex(
            r#"eval (defcommand "user.skip" "Go two tabs on"
                 (fn () (run "workspace.next_tab") (run "bn")))"#,
            cx
        ),
        "\"user.skip\""
    );
    // ...runs from the command line by its name...
    ex("user.skip", cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("c.txt")));
    // ...and from a key, like any other.
    ex(r#"eval (bind 'normal "<Space>j" "user.skip")"#, cx);
    cx.simulate_keystrokes("space j");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));

    // `run` has happened when it returns: the file is the new tab's.
    ex(
        r#"eval (defcommand 'user.where "Go on a tab and say where that is"
             (fn () (run "workspace.next_tab") (message (current-file))))"#,
        cx,
    );
    assert_eq!(ex("user.where", cx), path("c.txt"));
    assert_eq!(
        ex("eval (project-root)", cx),
        format!("{:?}", project.0.display().to_string())
    );

    // How it was invoked: the rest of the line, the !, the count.
    ex(
        r#"eval (defcommand "user.echo" "Say how this was invoked"
             (fn () (message command-arg "|" command-bang "|" command-count)))"#,
        cx,
    );
    assert_eq!(ex("user.echo! two words", cx), "two words|true|nil");
    assert_eq!(ex("user.echo", cx), "|false|nil");
    ex(r#"eval (bind 'normal "<Space>k" "user.echo")"#, cx);
    cx.simulate_keystrokes("3 space k");
    cx.run_until_parked();
    assert_eq!(message(cx), "|false|3");

    // One of the user's commands runs another on the spot, and has its own
    // invocation back afterwards.
    ex("eval (def log nil)", cx);
    ex(
        r#"eval (defcommand "user.inner" "Note the argument"
             (fn () (set! log (cons command-arg log))))"#,
        cx,
    );
    ex(
        r#"eval (defcommand "user.outer" "Note the argument, around user.inner"
             (fn ()
               (run "user.inner" "inner")
               (set! log (cons command-arg log))))"#,
        cx,
    );
    ex("user.outer outer", cx);
    assert_eq!(ex("eval log", cx), r#"("outer" "inner")"#);

    // Defining it again replaces it.
    ex(
        r#"eval (defcommand "user.skip" "Go back a tab" (fn () (run "bp")))"#,
        cx,
    );
    ex("user.skip", cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));

    // What goes wrong in a command is said, and the editor carries on.
    ex(r#"eval (defcommand "user.bad" "Fail" (fn () (car 5)))"#, cx);
    assert!(ex("user.bad", cx).ends_with("expected a list, got 5"));
    ex(
        r#"eval (defcommand "user.loop" "Never end" (fn () (run "user.loop")))"#,
        cx,
    );
    assert!(ex("user.loop", cx).ends_with("calls nested too deeply"));
    assert_eq!(ex("eval (+ 1 2)", cx), "3");

    // Names that are taken, and names of nothing.
    assert_eq!(
        ex(r#"eval (defcommand "workspace.save" "Mine" (fn () 1))"#, cx),
        "1:1: workspace.save is one of the editor's own commands"
    );
    assert_eq!(
        ex(
            r#"eval (defcommand "motion.word_forward" "Mine" (fn () 1))"#,
            cx
        ),
        "1:1: motion.word_forward is one of the editor's own commands"
    );
    assert_eq!(
        ex(r#"eval (defcommand "w" "Mine" (fn () 1))"#, cx),
        "1:1: w is what the command line calls workspace.save"
    );
    assert_eq!(
        ex(r#"eval (defcommand "user.x" "Mine" 5)"#, cx),
        "1:1: expected a function, got 5"
    );
    assert_eq!(
        ex(r#"eval (run "no.such")"#, cx),
        "1:1: there is no command no.such"
    );

    // With no workspace behind a program, as for the init file, a command
    // can be defined but an editor's own cannot be run.
    let (defined, ran) = cx.update(|_, cx| {
        let source = r#"(defcommand "user.later" "For later" (fn () 1))"#;
        let defined = crate::script::eval(source, cx);
        let ran = crate::script::eval(r#"(run "workspace.save")"#, cx);
        (defined, ran.unwrap_err().to_string())
    });
    assert_eq!(defined, Ok(med_lisp::Value::from("user.later")));
    assert_eq!(ran, "1:1: there is no window to run workspace.save in");
}

#[gpui::test]
fn the_user_defines_panels(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    // Type `:eval <source>` on one line, and read the status bar.
    let eval = |source: &str, cx: &mut VisualTestContext| {
        let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
        cx.simulate_keystrokes(":");
        cx.simulate_input(&format!("eval {source}"));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };
    let click = |x: f32, y: f32, cx: &mut VisualTestContext| {
        cx.simulate_click(point(px(x), px(y)), Modifiers::none());
        cx.run_until_parked();
    };
    let width = cx.update(|window, _| f32::from(window.viewport_size().width));

    // A sidebar on the right. What it shows is the program's own: a count
    // its click adds to.
    eval("(def clicks 0)", cx);
    eval("(def seen nil)", cx);
    assert_eq!(
        eval(
            r#"(defpanel 'counter :at 'right :width 200
                 (fn (panel)
                   (col :width 'full :height 'full
                     (text (str "clicked " clicks) :height 200
                           :on-click (fn ()
                                       (set! clicks (+ clicks 1))
                                       (set! seen (get panel :file)))))))"#,
            cx
        ),
        "\"counter\""
    );
    // It is not up until it is shown: a click where it will be lands in
    // the editor.
    assert_eq!(eval("(panel-shown? 'counter)", cx), "false");
    click(width - 100., 100., cx);
    assert_eq!(eval("clicks", cx), "0");
    assert_eq!(eval("(show-panel 'counter)", cx), "true");
    click(width - 100., 100., cx);
    click(width - 100., 100., cx);
    assert_eq!(eval("clicks", cx), "2");
    // The panel was told which file is being edited.
    assert_eq!(
        eval("seen", cx),
        format!("{:?}", project.path("a.txt").display().to_string())
    );

    // On the left a panel goes before the editor, and `:shown` has it up
    // from the start.
    eval(
        r#"(defpanel 'beside :at 'left :width 100 :shown true
             (fn (panel)
               (col :width 'full :height 'full
                 (text "x" :height 30 :on-click (fn () (set! clicks 100))))))"#,
        cx,
    );
    click(50., TITLE + 15., cx);
    assert_eq!(eval("clicks", cx), "100");

    // A list of the program's, a row to a value. It is to the right of
    // the first panel: they are drawn in the order they were defined.
    eval("(def picked nil)", cx);
    eval(
        r#"(defpanel 'letters :at 'right :width 120 :shown true
             (fn (panel)
               (rows '("a" "b" "c") :width 'full :height 90
                 (fn (letter)
                   (row :width 'full :height 30
                        :on-click (fn () (set! picked letter))
                     (text letter))))))"#,
        cx,
    );
    click(width - 60., TITLE + 30. + 15., cx);
    assert_eq!(eval("picked", cx), "\"b\"");
    click(width - 120. - 100., 100., cx);
    assert_eq!(eval("clicks", cx), "101");

    // set-layout changes how a panel of the user's looks, too.
    eval(
        r#"(set-layout 'counter
             (fn (panel)
               (col :width 'full :height 'full
                 (text "reset" :height 30 :on-click (fn () (set! clicks 0))))))"#,
        cx,
    );
    click(width - 120. - 100., TITLE + 15., cx);
    assert_eq!(eval("clicks", cx), "0");

    // A popup, over the editor. Its own click can put it away...
    eval("(def answer nil)", cx);
    eval(
        r#"(defpanel 'ask :at 'popup :width 300
             (fn (panel)
               (col (text "Yes" :height 30
                          :on-click (fn () (set! answer "yes") (hide-panel 'ask))))))"#,
        cx,
    );
    eval("(show-panel 'ask)", cx);
    click(width / 2., 72. + 15., cx);
    assert_eq!(eval("answer", cx), "\"yes\"");
    assert_eq!(eval("(panel-shown? 'ask)", cx), "false");
    // ...and so can escape, or a click beside it. Sidebars stay.
    eval("(show-panel 'ask)", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(eval("(panel-shown? 'ask)", cx), "false");
    assert_eq!(eval("(toggle-panel 'ask)", cx), "true");
    click(width / 2., 400., cx);
    assert_eq!(eval("(panel-shown? 'ask)", cx), "false");
    assert_eq!(eval("(panel-shown? 'counter)", cx), "true");
    // With no popup up, escape is the editor's as before.
    cx.simulate_keystrokes("i z escape x");
    assert_eq!(text(&ws, cx), "a\n");

    // At the bottom a panel goes across the window, under the sidebars
    // and above the status bar, and is as tall as what is in it. The
    // keyboard is not its concern: escape is still the editor's.
    let height = cx.update(|window, _| f32::from(window.viewport_size().height));
    eval(
        r#"(defpanel 'strip :at 'bottom
             (fn (panel)
               (col :width 'full
                 (text "below" :height 40 :on-click (fn () (set! answer "below"))))))"#,
        cx,
    );
    click(50., height - 26. - 20., cx);
    assert_eq!(eval("answer", cx), "\"yes\"");
    eval("(show-panel 'strip)", cx);
    click(50., height - 26. - 20., cx);
    assert_eq!(eval("answer", cx), "\"below\"");
    // Above it the sidebar is as it was.
    eval("(set! answer nil)", cx);
    click(50., height - 26. - 60., cx);
    assert_eq!(eval("answer", cx), "nil");
    cx.simulate_keystrokes("i z escape x");
    assert_eq!(text(&ws, cx), "a\n");
    assert_eq!(eval("(panel-shown? 'strip)", cx), "true");
    eval("(hide-panel 'strip)", cx);

    // Mistakes are explained.
    assert_eq!(
        eval("(show-panel 'nope)", cx),
        "1:1: nope is not a panel (counter beside letters ask strip)"
    );
    assert_eq!(
        eval("(defpanel 'x :at 'top (fn (panel) (col)))", cx),
        "1:1: at: expected 'left, 'right, 'bottom, 'popup, 'full, 'status, 'title, got top"
    );
    assert_eq!(
        eval("(defpanel 'x :colour 1 (fn (panel) (col)))", cx),
        "1:1: colour is not an option of defpanel (at width shown contexts)"
    );
    assert_eq!(
        eval("(defpanel 'x 5)", cx),
        "1:1: defpanel: expected a function, last, got 5"
    );
    assert_eq!(
        eval("(set-layout 'x (fn (panel) (col)))", cx),
        "1:1: x is not a panel (counter beside letters ask strip)"
    );

    // Running the init file again forgets the panels it did not define
    // this time. One it does define is up if it was up before.
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.mlisp"), cx));
    assert!(!ran);
    cx.run_until_parked();
    assert_eq!(
        eval("(panel-shown? 'counter)", cx),
        "1:1: counter is not a panel ()"
    );
    eval("(defpanel 'counter (fn (panel) (col)))", cx);
    assert_eq!(eval("(panel-shown? 'counter)", cx), "true");
    eval("(defpanel 'fresh (fn (panel) (col)))", cx);
    assert_eq!(eval("(panel-shown? 'fresh)", cx), "false");
}

/// The init file is `init.mlisp`; an `init.lisp`, as it was named before,
/// is run only while there is no `init.mlisp`.
#[test]
fn finds_the_init_file_by_either_name() {
    let dir = std::env::temp_dir().join(format!("med-init-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let find = || crate::script::init_file_in(&dir);
    assert_eq!(find(), dir.join("init.mlisp"));
    std::fs::write(dir.join("init.lisp"), "").unwrap();
    assert_eq!(find(), dir.join("init.lisp"));
    std::fs::write(dir.join("init.mlisp"), "").unwrap();
    assert_eq!(find(), dir.join("init.mlisp"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn runs_an_init_file(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        (
            "config/init.mlisp",
            "; My keys.\n\
             (defn leader (key command)\n\
             \x20 (bind 'normal (str \"<Space>\" key) command))\n\
             (leader \"n\" \"workspace.next_tab\")\n\
             (set-option 'font-size 20)\n\
             (leader \"p\" \"workspace.previous_tab\")\n\
             (set-option 'font-size 30)\n",
        ),
    ]);
    let (ws, cx) = open(&project, &["a.txt", "b.txt"], cx);

    let init = project.path("config/init.mlisp");
    let said = cx.update(|_, cx| {
        crate::script::load_file(&init, cx);
        crate::script::take_message(cx)
    });
    // The sixth line names a command that does not exist, which the
    // function it calls finds out...
    assert_eq!(
        said,
        Some(format!(
            "{}:3:3: there is no command workspace.previous_tab (from 6:1)",
            init.display()
        ))
    );
    // ...and what came before it took effect, what came after did not.
    cx.simulate_keystrokes("space n");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.update(|_, cx| assert_eq!(cx.global::<crate::Settings>().font_size, 20.));

    // A file that is not there is said to be so.
    let missing = project.path("config/nope.mlisp");
    let said = cx.update(|_, cx| {
        crate::script::load_file(&missing, cx);
        crate::script::take_message(cx)
    });
    assert!(
        said.unwrap()
            .starts_with(&format!("{}: ", missing.display()))
    );
    assert_eq!(cx.update(|_, cx| crate::script::take_message(cx)), None);
}

#[gpui::test]
fn panels_take_the_keyboard(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "abcdef\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    // Type `:eval <source>` on one line, and read the status bar.
    let eval = |source: &str, cx: &mut VisualTestContext| {
        let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
        cx.simulate_keystrokes(":");
        cx.simulate_input(&format!("eval {source}"));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };

    // A screen in place of the editor, with keys of its own: which table
    // they are looked up in is the program's to say, key by key.
    lisp("(def log nil) (def deep false)", cx);
    eval(
        r#"(defpanel 'screen :at 'full
             :contexts (fn () (if deep '(screen.deep screen) '(screen)))
             (fn (panel)
               (col :width 'full :height 'full
                 (text (if (get panel :focused) "here" "away")))))"#,
        cx,
    );
    eval(
        r#"(do (defn note (name)
                 (defcommand (str "user." name) (str "Note " name)
                   (fn () (set! log (cons name log)))))
               (map note '("a" "b" "gg" "dive"))
               (defcommand "user.leave" "Put the screen away"
                 (fn () (hide-panel 'screen)))
               (defcommand "user.enter" "Put the screen up"
                 (fn () (focus-panel 'screen)))
               (bind 'screen "a" "user.a")
               (bind 'screen "gg" "user.gg")
               (bind 'screen "d" "user.dive")
               (bind 'screen "q" "user.leave")
               (bind 'screen.deep "a" "user.b")
               (bind 'normal "<Space>s" "user.enter"))"#,
        cx,
    );
    assert_eq!(lisp("(panel-shown? 'screen)", cx), "false");
    cx.simulate_keystrokes("space s");
    cx.run_until_parked();
    assert!(shown("screen", cx));
    // The keys are the panel's now, not the editor's.
    cx.simulate_keystrokes("a x g g");
    cx.run_until_parked();
    assert_eq!(lisp("log", cx), r#"("gg" "a")"#);
    assert_eq!(text(&ws, cx), "abcdef\n");
    // A table named first is asked first; what it does not know, the
    // next one may.
    lisp("(set! deep true)", cx);
    cx.simulate_keystrokes("a g g");
    cx.run_until_parked();
    assert_eq!(lisp("log", cx), r#"("gg" "b" "gg" "a")"#);
    // Put away, it gives the keyboard back to the editor.
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(!shown("screen", cx));
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "bcdef\n");

    // A popup with an input: the keyboard goes to the input, what is
    // typed is the program's to hear of, and enter hands it over.
    lisp("(def typed nil) (def sent nil)", cx);
    eval(
        r#"(defpanel 'ask :at 'popup :width 300
             (fn (panel)
               (col :width 'full
                 (input 'answer :placeholder "Say"
                        :on-change (fn (text) (set! typed text))
                        :on-submit (fn (text)
                                     (set! sent text)
                                     (hide-panel 'ask))))))"#,
        cx,
    );
    eval(
        r#"(do (defcommand "user.ask" "Ask" (fn () (focus-panel 'ask)))
               (defcommand "user.up" "Note up" (fn () (set! log 'up)))
               (bind 'ask "<Up>" "user.up")
               (bind 'normal "<Space>a" "user.ask"))"#,
        cx,
    );
    cx.simulate_keystrokes("space a");
    cx.run_until_parked();
    cx.simulate_input("hello");
    cx.run_until_parked();
    assert_eq!(lisp("typed", cx), "\"hello\"");
    assert_eq!(text(&ws, cx), "bcdef\n");
    // A key of the panel's table is the panel's even while typing.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(lisp("log", cx), "up");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(lisp("sent", cx), "\"hello\"");
    assert!(!shown("ask", cx));
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "cdef\n");

    // A program reads what is in an input, and puts text there.
    lisp(
        r#"(do (defcommand "user.read" "Note what is typed"
                 (fn () (set! log (input-text 'ask 'answer))))
               (defcommand "user.fill" "Type for the user"
                 (fn () (set-input 'ask 'answer "bye")))
               (bind 'ask "<C-r>" "user.read")
               (bind 'ask "<C-f>" "user.fill"))"#,
        cx,
    );
    cx.simulate_keystrokes("space a");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-r");
    cx.run_until_parked();
    assert_eq!(lisp("log", cx), "\"hello\"");
    // What a program puts there is not typing: it is not told of it.
    cx.simulate_keystrokes("ctrl-f");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-r");
    cx.run_until_parked();
    assert_eq!(lisp("(list log typed)", cx), r#"("bye" "hello")"#);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(lisp("sent", cx), "\"bye\"");
}

#[gpui::test]
fn programs_follow_the_keys_being_typed(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "abc def\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let error = |source: &str, cx: &mut VisualTestContext| {
        cx.update(|_, cx| crate::script::eval(source, cx).unwrap_err().to_string())
    };
    let wait = |milliseconds: u64, cx: &mut VisualTestContext| {
        cx.executor()
            .advance_clock(Duration::from_millis(milliseconds));
        cx.run_until_parked();
    };

    // The keys that can follow others: a command with what it says of
    // itself, the start of longer bindings, keys that stand for keys.
    lisp(
        r#"(do (defcommand "user.find" "Find something" (fn () (set! found (+ found 1))))
               (def found 0)
               (unbind 'normal "<Space>q")
               (unbind 'normal "<Space>w")
               (unbind '(normal visual) "<Space>a")
               (unbind 'normal "<Space>=")
               (unbind 'normal "<Space>r")
               (unbind 'normal "<Space>m")
               (unbind 'normal "<Space>t")
               (bind 'normal "<Space>f" "user.find")
               (bind 'normal "<Space>gg" "user.find")
               (bind-keys 'normal "<Space>y" "y$"))"#,
        cx,
    );
    assert_eq!(
        lisp(r#"(bindings 'normal "<space>")"#, cx),
        "((:key \"f\" :keys \"<Space>f\" :command \"user.find\" \
           :description \"Find something\" :to nil :prefix false) \
          (:key \"g\" :keys \"<Space>g\" :command nil \
           :description nil :to nil :prefix true) \
          (:key \"y\" :keys \"<Space>y\" :command nil \
           :description nil :to \"y$\" :prefix false))"
    );
    // The engine's own commands go by their names alone. In the order of
    // their notation.
    assert_eq!(
        lisp(
            r#"(map (fn (next) (list (get next :key) (get next :command)))
                    (bindings 'normal "z"))"#,
            cx
        ),
        r#"(("-" "view.bottom") ("." "view.center") ("<CR>" "view.top") ("b" "view.bottom") ("t" "view.top") ("z" "view.center"))"#
    );
    // With no keys, the first key of every binding.
    assert_eq!(
        lisp(
            r#"(get (car (filter (fn (next) (= (get next :key) "<Space>")) (bindings 'normal)))
                    :prefix)"#,
            cx
        ),
        "true"
    );
    assert_eq!(lisp(r#"(bindings 'normal "q")"#, cx), "nil");
    assert_eq!(lisp(r#"(bindings 'normal "<Space>f")"#, cx), "nil");
    assert_eq!(
        error("(bindings 'app)", cx),
        "1:1: the shortcuts in app are the window system's, and cannot be listed"
    );
    assert!(error("(bindings 'nomal)", cx).starts_with("1:1: nomal is neither a mode"));

    // A program is told what a half-typed command waits for, and when it
    // waits no more.
    lisp(
        "(do (def seen nil)
             (add-hook 'keys-pending (fn (pending) (set! seen (cons pending seen)))))",
        cx,
    );
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(
        lisp("seen", cx),
        r#"((:tables ("normal") :keys "<Space>" :typed "<Space>" :awaiting nil))"#
    );
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert_eq!(
        lisp("(car seen)", cx),
        r#"(:tables ("normal") :keys "<Space>g" :typed "<Space>g" :awaiting nil)"#
    );
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert_eq!(
        lisp("(list (car seen) (length seen) found)", cx),
        "(nil 3 1)"
    );
    // A key that is a command by itself is nothing to wait for.
    cx.simulate_keystrokes("l h");
    cx.run_until_parked();
    assert_eq!(lisp("(length seen)", cx), "3");

    // After an operator any motion can follow; some keys are taken as
    // they are.
    lisp("(set! seen nil)", cx);
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        lisp("(car seen)", cx),
        r#"(:tables ("operator") :keys "" :typed "d" :awaiting nil)"#
    );
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert_eq!(
        lisp("(car seen)", cx),
        r#"(:tables ("operator") :keys "" :typed "di" :awaiting text-object)"#
    );
    cx.simulate_keystrokes("w");
    cx.run_until_parked();
    assert_eq!(lisp("(list (car seen) (length seen))", cx), "(nil 3)");
    assert_eq!(text(&ws, cx), " def\n");

    // The wait for a longer binding ends by itself, unless it is told not
    // to, or to take another time over it.
    lisp("(set! seen nil)", cx);
    cx.simulate_keystrokes("space");
    wait(999, cx);
    assert_eq!(lisp("(length seen)", cx), "1");
    assert_eq!(cursor(&ws, cx), (1, 1));
    wait(1, cx);
    assert_eq!(lisp("(list (car seen) (length seen))", cx), "(nil 2)");
    assert_eq!(cursor(&ws, cx), (1, 2));

    lisp("(set-option 'key-timeout nil)", cx);
    cx.simulate_keystrokes("space");
    wait(60_000, cx);
    assert_eq!(lisp("(get (car seen) :keys)", cx), "\"<Space>\"");
    assert_eq!(cursor(&ws, cx), (1, 2));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(lisp("(list (car seen) (length seen))", cx), "(nil 4)");
    assert_eq!(cursor(&ws, cx), (1, 2));

    lisp("(set-option 'key-timeout 50)", cx);
    cx.simulate_keystrokes("space");
    wait(50, cx);
    assert_eq!(cursor(&ws, cx), (1, 3));
    assert_eq!(
        error("(set-option 'key-timeout -1)", cx),
        "1:1: -1 is no time to wait, in milliseconds"
    );
    lisp("(set-option 'key-timeout 1000)", cx);

    // A function is called later, unless that is called off first.
    lisp(
        "(do (def fired nil)
             (def slow (after 200 (fn () (set! fired (cons 'slow fired)))))
             (def quick (after 100 (fn () (set! fired (cons 'quick fired))))))",
        cx,
    );
    wait(150, cx);
    assert_eq!(lisp("fired", cx), "(quick)");
    assert_eq!(
        lisp("(list (cancel slow) (cancel slow))", cx),
        "(true false)"
    );
    assert_eq!(
        lisp("(list (cancel quick) (cancel nil))", cx),
        "(false false)"
    );
    wait(500, cx);
    assert_eq!(lisp("fired", cx), "(quick)");
    assert_eq!(
        error("(after -1 (fn () 1))", cx),
        "1:1: -1 is no time to wait, in milliseconds"
    );
    assert_eq!(error("(after 10 5)", cx), "1:1: expected a function, got 5");

    // Together: the keys that can come next, shown once the user has
    // hesitated, and put away when the command is done with.
    lisp(
        r#"(do (def hints nil)
               (def hint-timer nil)
               (defpanel 'hints :at 'bottom
                 (fn (panel)
                   (col (map (fn (next) (text (get next :keys))) hints))))
               (add-hook 'keys-pending
                 (fn (pending)
                   (cancel hint-timer)
                   (hide-panel 'hints)
                   (when (and pending (not (= (get pending :keys) "")))
                     (set! hint-timer
                       (after 300
                         (fn ()
                           (set! hints (bindings (car (get pending :tables))
                                                 (get pending :keys)))
                           (show-panel 'hints))))))))"#,
        cx,
    );
    cx.simulate_keystrokes("space");
    wait(299, cx);
    assert!(!shown("hints", cx));
    wait(1, cx);
    assert!(shown("hints", cx));
    assert_eq!(
        lisp("(map (fn (next) (get next :keys)) hints)", cx),
        r#"("<Space>f" "<Space>g" "<Space>y")"#
    );
    // The keyboard is still the editor's.
    cx.simulate_keystrokes("f");
    cx.run_until_parked();
    assert!(!shown("hints", cx));
    assert_eq!(lisp("found", cx), "2");
    // Typed without hesitating, nothing is shown at all.
    cx.simulate_keystrokes("space f");
    wait(500, cx);
    assert!(!shown("hints", cx));
    assert_eq!(lisp("found", cx), "3");
    // Escape is the editor's while they are up: it abandons the command,
    // and with that they go.
    cx.simulate_keystrokes("space");
    wait(300, cx);
    assert!(shown("hints", cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("hints", cx));
    assert_eq!(lisp("(car seen)", cx), "nil");
    wait(500, cx);
    assert!(!shown("hints", cx));

    // A panel's keys are followed as an editor's are: in its tables.
    lisp("(set! seen nil)", cx);
    cx.simulate_keystrokes(":");
    cx.simulate_input(
        "eval (do (defpanel 'screen :at 'full \
           :contexts '(screen.deep elsewhere screen) \
           (fn (panel) (text \"screen\"))) \
         (bind 'screen \"gg\" \"user.find\") \
         (bind 'screen.deep \"gd\" \"user.find\") \
         (focus-panel 'screen))",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(shown("screen", cx));
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert_eq!(
        lisp("(car seen)", cx),
        r#"(:tables ("screen.deep" "screen") :keys "g" :typed "g" :awaiting nil)"#
    );
    assert_eq!(
        lisp(
            r#"(map (fn (table) (map (fn (next) (get next :keys)) (bindings table "g")))
                    (get (car seen) :tables))"#,
            cx
        ),
        r#"(("gd") ("gg"))"#
    );
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert_eq!(
        lisp("(list (car seen) (length seen) found)", cx),
        "(nil 2 4)"
    );
    // A key that is no binding ends the wait too.
    cx.simulate_keystrokes("g x");
    cx.run_until_parked();
    assert_eq!(
        lisp("(list (car seen) (length seen) found)", cx),
        "(nil 4 4)"
    );

    // Running the init file again calls off what was to be called.
    lisp("(after 100 (fn () (set! fired 'late)))", cx);
    cx.update(|_, cx| crate::script::run_init(&project.path("nope.mlisp"), cx));
    wait(500, cx);
    assert_eq!(lisp("fired", cx), "(quick)");
}

#[gpui::test]
fn programs_load_files_and_hear_of_changes(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        (
            "config/init.mlisp",
            "(def seen nil)\n\
             (load \"lib/hooks.mlisp\")\n\
             (load \"lib/broken.mlisp\")\n",
        ),
        (
            "config/lib/hooks.mlisp",
            "(add-hook 'opened (fn () (set! seen (cons 'opened seen))))\n\
             (add-hook 'files-changed (fn () (set! seen (cons 'changed seen))))\n\
             (load \"more.mlisp\")\n",
        ),
        ("config/lib/more.mlisp", "(def more 'loaded)\n"),
        ("config/lib/broken.mlisp", "\n(car 5)\n"),
    ]);
    let init = project.path("config/init.mlisp");
    let said = cx.update(|cx| {
        crate::init(cx);
        cx.global_mut::<crate::Settings>().lsp = false;
        bind_example_keys(cx);
        assert!(crate::script::run_init(&init, cx));
        crate::script::take_message(cx)
    });
    // A file is looked for beside the one that loads it, and what goes
    // wrong in it is said with its own name and line.
    assert_eq!(
        said,
        Some(format!(
            "{}:3:1: {}:2:1: expected a list, got 5",
            init.display(),
            project.path("config/lib/broken.mlisp").display()
        ))
    );
    let root = project.0.clone();
    let files = vec![project.path("a.txt")];
    let (ws, cx) = cx.add_window_view(|window, cx| Workspace::new(root, files, window, cx));
    cx.run_until_parked();
    assert_eq!(lisp("more", cx), "loaded");

    // The window coming up is heard of, and so is a file being written.
    assert_eq!(lisp("seen", cx), "(opened)");
    cx.simulate_keystrokes("x : w enter");
    cx.run_until_parked();
    assert_eq!(lisp("seen", cx), "(changed opened)");
    assert_eq!(text(&ws, cx), "\n");

    // What is not a hook, or not a file, is said to be so.
    let error = |source: &str, cx: &mut VisualTestContext| {
        cx.update(|_, cx| crate::script::eval(source, cx).unwrap_err().to_string())
    };
    assert_eq!(
        error("(add-hook 'closed (fn () 1))", cx),
        "1:1: closed is not a hook (opened files-changed locations keys-pending \
         buffer-entered buffer-changed cursor-moved mode-changed diagnostics-changed \
         terminal-exited)"
    );
    assert!(
        error("(load \"/nowhere/at/all.mlisp\")", cx).starts_with("1:1: /nowhere/at/all.mlisp: ")
    );

    // Running the init file again forgets the hooks it set before.
    cx.update(|_, cx| crate::script::run_init(&project.path("config/nope.mlisp"), cx));
    cx.simulate_keystrokes("i z escape : w enter");
    cx.run_until_parked();
    assert_eq!(lisp("seen", cx), "(changed opened)");
}

#[gpui::test]
fn a_popup_says_how_to_get_keys(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let (ws, cx) = open_window(false, &[], &project, &["a.txt"], cx);
    let shown = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.welcome_shown(cx))
    };
    let reason = cx
        .update(|_, cx| crate::defaults::no_keys_reason(cx))
        .unwrap();
    assert!(reason.contains("init.mlisp"), "{reason}");
    assert!(shown(&ws, cx));

    // Out of the way while `:` is typed, back after.
    cx.simulate_keystrokes(":");
    assert!(!shown(&ws, cx));
    cx.simulate_keystrokes("escape");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert!(shown(&ws, cx));
    // Escape puts it away.
    cx.simulate_keystrokes("escape");
    assert!(!shown(&ws, cx));
}

#[gpui::test]
fn the_popup_goes_once_keys_are_bound(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let (ws, cx) = open_window(false, &[], &project, &["a.txt"], cx);
    let shown = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.welcome_shown(cx))
    };
    assert!(shown(&ws, cx));
    let config = project.path("config");
    ws.update_in(cx, |ws, window, cx| {
        ws.install_default_config(&config, false, window, cx)
    });
    cx.run_until_parked();
    assert!(!shown(&ws, cx));
    assert_eq!(cx.update(|_, cx| crate::defaults::no_keys_reason(cx)), None);
}

#[gpui::test]
fn installs_keys_into_an_editor_that_has_none(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\n")]);
    let (ws, cx) = open_window(false, &[], &project, &["a.txt"], cx);
    let said = ws.read_with(cx, |ws, _| ws.message.clone()).unwrap();
    assert!(said.contains(":install_default_config"), "{said}");

    // No key does anything but `:`.
    cx.simulate_keystrokes("j d d");
    assert_eq!(text(&ws, cx), "one\ntwo\n");
    assert_eq!(cursor(&ws, cx), (1, 1));
    cx.simulate_keystrokes(":");
    assert_eq!(mode(&ws, cx), Mode::Command);
    cx.simulate_keystrokes("escape");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    // And what is typed after it runs.
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    let said = ws.read_with(cx, |ws, _| ws.message.clone()).unwrap();
    assert!(said.contains("written"), "{said}");

    let config = project.path("config");
    ws.update_in(cx, |ws, window, cx| {
        ws.install_default_config(&config, false, window, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        ws.read_with(cx, |ws, _| ws.message.clone()).as_deref(),
        Some(format!("Wrote the example configuration to {}", config.display()).as_str())
    );
    assert!(config.join("init.mlisp").is_file());
    cx.simulate_keystrokes("j d d");
    assert_eq!(text(&ws, cx), "one\n");

    // A second time, nothing is overwritten unless asked.
    fs::write(config.join("keys.mlisp"), "; mine\n").unwrap();
    ws.update_in(cx, |ws, window, cx| {
        ws.install_default_config(&config, false, window, cx)
    });
    let said = ws.read_with(cx, |ws, _| ws.message.clone()).unwrap();
    assert!(said.contains(":install_default_config!"), "{said}");
    assert_eq!(
        fs::read_to_string(config.join("keys.mlisp")).unwrap(),
        "; mine\n"
    );
}

#[gpui::test]
fn the_examples_load_together(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let init = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/init.mlisp");
    let said = cx.update(|cx| {
        crate::init(cx);
        cx.global_mut::<crate::Settings>().lsp = false;
        assert!(crate::script::run_init(&init, cx));
        crate::script::take_message(cx)
    });
    assert_eq!(said, None);
    let root = project.0.clone();
    let files = vec![project.path("a.txt")];
    let (ws, cx) = cx.add_window_view(|window, cx| Workspace::new(root, files, window, cx));
    cx.run_until_parked();
    assert_eq!(ws.read_with(cx, |ws, _| ws.message.clone()), None);
    let panels = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let panels = cx.global::<UserPanels>();
            panels.names().map(String::from).collect::<Vec<_>>()
        })
    };
    assert_eq!(
        panels(cx),
        [
            "oil-confirm",
            "picker",
            "git",
            "git-prompt",
            "which-key",
            "statusline",
            "titlebar",
            "titlebar-branches"
        ]
    );

    // `:help` shows what every command says of itself: the editor's own
    // and the examples' all say something.
    let silent: Vec<String> = cx.update(|_, cx| {
        let commands = cx.global::<crate::commands::Commands>().all();
        commands
            .iter()
            .filter(|command| command.description.trim().is_empty())
            .map(|command| command.name.to_string())
            .collect()
    });
    assert_eq!(silent, Vec::<String>::new(), "commands with no description");

    // space e lists the file's directory, as oil.nvim, and the leader
    // keys work from the listing as they do from a file.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "a.txt\nb.txt\n");
    // g ? lists all of the listing's keys.
    cx.simulate_keystrokes("g ?");
    cx.run_until_parked();
    assert!(shown("which-key", cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    assert!(shown("picker", cx));
    cx.simulate_input("b.t");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    // Put away without choosing, the picker leaves the keyboard in the
    // listing it was called from, and a file opened from there takes the
    // listing's place.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("picker", cx));
    cx.simulate_keystrokes("g g enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    let titles = tab_titles(&ws, cx);
    assert!(
        !titles.iter().any(|title| title.starts_with("oil://")),
        "{titles:?}"
    );

    // Run again, the init file leaves one of each panel, and they still
    // have a project to show.
    cx.simulate_keystrokes(":");
    cx.simulate_input("workspace.reload_init");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default()),
        format!("Ran {}", init.display())
    );
    assert_eq!(
        panels(cx),
        [
            "oil-confirm",
            "picker",
            "git",
            "git-prompt",
            "which-key",
            "statusline",
            "titlebar",
            "titlebar-branches"
        ]
    );
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "a.txt\nb.txt\n");
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    cx.simulate_keystrokes("space g");
    cx.run_until_parked();
    assert!(shown("git", cx));
    // This is no repository, and the screen says so rather than fail.
    assert!(wait_for(cx, |cx| lisp("git-repo", cx) == "false"));
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(!shown("git", cx));
}

#[gpui::test]
fn shows_the_keys_that_can_come_next(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open_with(&[TREE, WHICH_KEY], &project, &["a.txt"], cx);
    let wait = |milliseconds: u64, cx: &mut VisualTestContext| {
        cx.executor()
            .advance_clock(Duration::from_millis(milliseconds));
        cx.run_until_parked();
    };
    // What is listed for a key: what it does, and what goes on from it.
    let listed = |key: &str, cx: &mut VisualTestContext| {
        let find = format!(
            r#"(let ((item (car (filter (fn (item) (= (get item :key) "{key}"))
                                        which-key-items))))
                 (list (get item :label) (get item :group)))"#
        );
        lisp(&find, cx)
    };

    // Nothing until the user has hesitated over a half-typed command.
    cx.simulate_keystrokes("j g");
    wait(299, cx);
    assert!(!shown("which-key", cx));
    wait(1, cx);
    assert!(shown("which-key", cx));
    assert_eq!(lisp("which-key-typed", cx), "\"g\"");
    // A command of the editor's says what it does; one of the engine's
    // has its name.
    assert_eq!(
        listed("r", cx),
        r#"("List the references to the symbol under the cursor" nil)"#
    );
    assert_eq!(listed("g", cx), r#"("motion.goto_first_line" nil)"#);
    // The next key is the editor's, and the list goes when it is done.
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(cursor(&ws, cx), (1, 1));
    // Typed without a pause, nothing is shown at all.
    cx.simulate_keystrokes("j g g");
    wait(1000, cx);
    assert!(!shown("which-key", cx));
    assert_eq!(cursor(&ws, cx), (1, 1));

    // A key that is a command by itself waits for the next one, however
    // long that takes, and escape gives up on it.
    cx.simulate_keystrokes("space");
    wait(60_000, cx);
    assert!(shown("which-key", cx));
    assert_eq!(cursor(&ws, cx), (1, 1));
    assert_eq!(
        listed("w", cx),
        r#"("Write the file; given a name, write it under that name" nil)"#
    );
    assert_eq!(
        listed("e", cx),
        r#"("Go to the file tree, or put it away" nil)"#
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(cursor(&ws, cx), (1, 1));

    // Keys that only start longer bindings are a group: so many keys, or
    // the name it was given. Keys that stand for keys say which.
    lisp(
        r#"(do (bind 'normal "<Space>gs" "workspace.save")
               (bind 'normal "<Space>gq" "workspace.close_tab")
               (bind 'normal "<Space>t" "workspace.next_tab")
               (bind 'normal "<Space>tt" "workspace.prev_tab")
               (bind-keys 'normal "<Space>y" "y$"))"#,
        cx,
    );
    cx.simulate_keystrokes("space");
    wait(300, cx);
    assert_eq!(listed("g", cx), r#"(nil "+2 keys")"#);
    assert_eq!(listed("t", cx), r#"("Go to the next tab" "+1 keys")"#);
    assert_eq!(listed("y", cx), r#"("→ y$" nil)"#);
    // Once up, the list follows the keys without waiting again.
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert!(shown("which-key", cx));
    assert_eq!(lisp("which-key-typed", cx), "\"<Space>g\"");
    assert_eq!(
        lisp("(map (fn (item) (get item :key)) which-key-items)", cx),
        r#"("q" "s")"#
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    lisp(r#"(which-key-name "<Space>g" "go")"#, cx);
    cx.simulate_keystrokes("space");
    wait(300, cx);
    assert_eq!(listed("g", cx), r#"(nil "+go")"#);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));

    // How it is laid out: down the columns, then across, and each key
    // drawn without complaint.
    assert_eq!(
        lisp("(which-key-split 4 2 '(1 2 3 4 5))", cx),
        "((1 2) (3 4) (5) nil)"
    );
    lisp("(map which-key-entry which-key-items)", cx);

    // A key that is taken as it is has nothing to list, nor has a count,
    // nor an operator: any motion can follow that.
    for keys in ["f", "\"", "2", "d"] {
        cx.simulate_keystrokes(keys);
        wait(1000, cx);
        assert!(!shown("which-key", cx), "{keys}");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    // The start of a motion after an operator has.
    cx.simulate_keystrokes("d g");
    wait(300, cx);
    assert!(shown("which-key", cx));
    assert_eq!(lisp("which-key-typed", cx), "\"dg\"");
    assert_eq!(listed("g", cx), r#"("motion.goto_first_line" nil)"#);
    assert_eq!(listed("r", cx), "(nil nil)");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    // Every motion is listed for an operator if that is wanted.
    lisp("(set! which-key-after-operator true)", cx);
    cx.simulate_keystrokes("d");
    wait(300, cx);
    assert!(shown("which-key", cx));
    assert_eq!(listed("w", cx), r#"("motion.word_forward" nil)"#);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(text(&ws, cx), "one\ntwo\nthree\n");

    // Not while text is typed, though a key is held back there too.
    lisp(r#"(bind 'insert "jk" "insert.escape")"#, cx);
    cx.simulate_keystrokes("i");
    cx.simulate_input("j");
    wait(1000, cx);
    assert!(!shown("which-key", cx));
    cx.simulate_input("k");
    cx.run_until_parked();
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert_eq!(text(&ws, cx), "one\ntwo\nthree\n");

    // A panel with the keyboard has its keys listed as an editor has.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert!(shown("tree", cx));
    cx.simulate_keystrokes("j g");
    wait(300, cx);
    assert!(shown("which-key", cx));
    assert_eq!(listed("g", cx), r#"("Select the first entry" nil)"#);
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(lisp("tree-selected", cx), "0");
}

#[gpui::test]
fn a_help_key_lists_every_key_at_once(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let help = r#"(which-key-help-key 'tree "?")
                  (which-key-help-key 'dired "?")"#;
    let (ws, cx) = open_with(&[TREE, WHICH_KEY, DIRED, help], &project, &["a.txt"], cx);
    let listed = |key: &str, cx: &mut VisualTestContext| {
        let find = format!(
            r#"(let ((item (car (filter (fn (item) (= (get item :key) "{key}"))
                                        which-key-items))))
                 (list (get item :label) (get item :group)))"#
        );
        lisp(&find, cx)
    };

    // In a listing: up without waiting, with the listing's keys, groups
    // and all.
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "a.txt\nb.txt\nc.txt\n");
    cx.simulate_keystrokes("?");
    cx.run_until_parked();
    assert!(shown("which-key", cx));
    assert_eq!(lisp("which-key-typed", cx), "\"?\"");
    assert_eq!(listed("q", cx), r#"("Close the listing" nil)"#);
    assert_eq!(listed("*", cx), r#"(nil "+6 keys")"#);
    // The key typed then does what it does in the listing, and the list
    // goes.
    cx.simulate_keystrokes("m");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(cursor(&ws, cx).0, 2);
    assert_eq!(
        lisp(r#"(dired-mark-of (car dired-states) "a.txt")"#, cx),
        "\"*\""
    );
    // So does a key of a group.
    cx.simulate_keystrokes("? * !");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(
        lisp(r#"(dired-mark-of (car dired-states) "a.txt")"#, cx),
        "nil"
    );
    // A key that is none of the listing's does what it does without `?`:
    // it is not searched for backwards, as vim's `?` would.
    cx.simulate_keystrokes("? j");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert_eq!(cursor(&ws, cx).0, 3);
    assert_eq!(text(&ws, cx), "a.txt\nb.txt\nc.txt\n");
    cx.simulate_keystrokes("k");
    // Escape gives up, and the listing is as it was.
    cx.simulate_keystrokes("?");
    cx.run_until_parked();
    assert!(shown("which-key", cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(cursor(&ws, cx).0, 2);
    assert_eq!(mode(&ws, cx), Mode::Normal);

    // In the tree, the same.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(eval_here(&ws, "(panel-focused? 'tree)", cx), "true");
    cx.simulate_keystrokes("?");
    cx.run_until_parked();
    assert!(shown("which-key", cx));
    assert_eq!(listed("G", cx), r#"("Select the last entry" nil)"#);
    cx.simulate_keystrokes("G");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(lisp("tree-selected", cx), "2");
    // Escape gives up the key, and leaves the tree as it is.
    cx.simulate_keystrokes("? escape");
    cx.run_until_parked();
    assert!(!shown("which-key", cx));
    assert_eq!(eval_here(&ws, "(panel-focused? 'tree)", cx), "true");
    assert_eq!(lisp("tree-selected", cx), "2");
}

#[gpui::test]
fn stages_and_commits_from_the_git_screen(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    project.git_init();
    project.git(&["add", "-A"]);
    project.git(&["commit", "-q", "-m", "first"]);
    fs::write(project.path("a.txt"), "one\ntwo\n").unwrap();
    let repo = med_git::Repo::discover(&project.0).unwrap();

    let (ws, cx) = open_with(&[GIT], &project, &["a.txt"], cx);
    // The letters git has for the file the keys act on.
    let code = |cx: &mut VisualTestContext| {
        lisp(
            "(let ((file (git-current))) (str (get file :index) (get file :worktree)))",
            cx,
        )
    };
    // Nothing is going on, and nothing is waiting to be drawn.
    let settled = |cx: &mut VisualTestContext| lisp("git-busy", cx) == "nil";

    cx.simulate_keystrokes("space g");
    cx.run_until_parked();
    assert!(shown("git", cx));
    assert!(wait_for(cx, |cx| code(cx) == "\" M\""));
    // The preview is the file's diff.
    assert!(wait_for(cx, |cx| {
        lisp("(member? \"+two\" git-preview)", cx) == "true"
    }));
    assert_eq!(lisp("git-preview-title", cx), "\"a.txt\"");

    // space stages the selected file.
    cx.simulate_keystrokes("space");
    assert!(wait_for(cx, |cx| code(cx) == "\"M \""));
    assert_eq!(repo.status().unwrap()[0].code(), "M ");
    assert!(wait_for(cx, settled));
    assert_eq!(lisp("git-message", cx), "(\"stage: done\" false)");

    // c opens the message prompt; enter commits.
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert!(shown("git-prompt", cx));
    cx.simulate_input("second commit");
    cx.simulate_keystrokes("enter");
    assert!(wait_for(cx, |cx| lisp("git-files", cx) == "nil"));
    assert!(!shown("git-prompt", cx));
    let log = repo.log(None, 10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "second commit");
    assert!(wait_for(cx, |cx| lisp("(length git-commits)", cx) == "2"));
    assert!(wait_for(cx, settled));

    // n in the branches panel creates and checks out a branch. The keys
    // are the screen's again once the prompt is gone.
    cx.simulate_keystrokes("3 n");
    cx.run_until_parked();
    assert_eq!(lisp("git-section", cx), "branches");
    cx.simulate_input("feature");
    cx.simulate_keystrokes("enter");
    assert!(wait_for(cx, |cx| {
        lisp("(get git-head :name)", cx) == "\"feature\""
    }));
    assert_eq!(repo.head().unwrap().name, "feature");
    assert!(wait_for(cx, settled));

    // d asks before it deletes a branch, and an n leaves it be.
    assert!(wait_for(cx, |cx| lisp("(length git-branches)", cx) == "2"));
    let on_other = "(not (get (git-current) :current))";
    if lisp(on_other, cx) == "false" {
        let down = lisp("(= (git-index 'branches) 0)", cx) == "true";
        cx.simulate_keystrokes(if down { "j" } else { "k" });
        cx.run_until_parked();
    }
    assert_eq!(lisp(on_other, cx), "true");
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        lisp("(get git-question :text)", cx),
        "\"Delete branch main?\""
    );
    // While it waits, the other keys do nothing.
    cx.simulate_keystrokes("2 n");
    cx.run_until_parked();
    assert_eq!(
        lisp("(list git-section git-question)", cx),
        "(branches nil)"
    );
    assert_eq!(repo.branches().unwrap().len(), 2);
    cx.simulate_keystrokes("d y");
    assert!(wait_for(cx, |cx| lisp("(length git-branches)", cx) == "1"));
    assert_eq!(repo.branches().unwrap().len(), 1);
    assert!(wait_for(cx, settled));

    // q closes the screen and returns to the editor.
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(!shown("git", cx));
    cx.simulate_keystrokes("shift-g");
    assert_eq!(cursor(&ws, cx), (2, 1));
}

#[gpui::test]
fn switches_branches_from_the_title_bar(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    project.git_init();
    project.git(&["add", "-A"]);
    project.git(&["commit", "-q", "-m", "first"]);
    project.git(&["branch", "feature"]);
    let repo = med_git::Repo::discover(&project.0).unwrap();

    let (ws, cx) = open_with(&[TITLEBAR], &project, &["a.txt"], cx);
    let branch = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let vars = ws.bar_vars(cx);
            let (_, value) = vars.iter().find(|(name, _)| *name == "branch").unwrap();
            value.to_string()
        })
    };
    assert!(wait_for(cx, |cx| branch(&ws, cx) == "\"main\""));

    // The branch is at the right of the title bar, and a click on it
    // lists every branch, the current one first.
    let width = cx.update(|window, _| f32::from(window.viewport_size().width));
    cx.simulate_click(point(px(width - 30.), px(TITLE / 2.)), Modifiers::none());
    assert!(wait_for(cx, |cx| shown("titlebar-branches", cx)));
    assert_eq!(
        lisp("(map (fn (b) (get b :name)) titlebar-branches)", cx),
        r#"("main" "feature")"#
    );

    // Choosing another checks it out, and the bar follows.
    eval_in(&ws, "(titlebar-checkout (car (cdr titlebar-branches)))", cx);
    assert!(!shown("titlebar-branches", cx));
    assert!(wait_for(cx, |cx| branch(&ws, cx) == "\"feature\""));
    assert_eq!(repo.head().unwrap().name, "feature");
}

#[gpui::test]
fn mouse_and_app_shortcuts(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.rs", "fn main() {\n    let value = 1;\n}\n"),
        ("b.rs", "// b\n"),
    ]);
    let (ws, cx) = open(&project, &["a.rs", "b.rs"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());

    // Clicking a character puts the cursor on it (hit-testing agrees with
    // where the text was painted).
    let target = editor.read_with(cx, |e, _| e.buf.line_start(1) + 8);
    let cell = editor
        .read_with(cx, |e, _| e.cell_origin(target))
        .expect("editor was painted");
    cx.simulate_click(point(cell.x + px(2.), cell.y + px(4.)), Modifiers::none());
    assert_eq!(cursor(&ws, cx), (2, 9));
    cx.simulate_keystrokes("c i w n a m e escape");
    assert_eq!(text(&ws, cx), "fn main() {\n    let name = 1;\n}\n");

    // ⌘S saves, ⌘W closes, ⌘⇧] switches tabs.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(project.read("a.rs"), "fn main() {\n    let name = 1;\n}\n");
    cx.simulate_keystrokes("cmd-shift-]");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.rs")));
    cx.simulate_keystrokes("cmd-w");
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs().len()), 1);
    assert_eq!(active_file(&ws, cx), Some(project.path("a.rs")));

    // ⌘V pastes the system clipboard, in insert mode and in normal mode.
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("// pasted\n".into()));
    cx.simulate_keystrokes("g g cmd-v");
    assert_eq!(
        text(&ws, cx),
        "// pasted\nfn main() {\n    let name = 1;\n}\n"
    );
    cx.simulate_keystrokes("shift-g shift-a cmd-v escape");
    assert_eq!(
        text(&ws, cx),
        "// pasted\nfn main() {\n    let name = 1;\n}// pasted\n\n"
    );
}

#[gpui::test]
fn keys_and_commands_are_data(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one two\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open_with(&[TREE], &project, &["a.txt", "b.txt"], cx);
    let message = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default())
    };

    // Nothing about a key is fixed: the tables can change while running.
    cx.update(|_, cx| {
        let keymap = &mut cx.global_mut::<crate::VimGlobal>().0.keymap;
        keymap.bind(MapMode::Normal, "<Space>n", "workspace.next_tab");
        keymap.bind(MapMode::Normal, "Q", "operator.delete");
        assert!(keymap.unbind(MapMode::Normal, "gt"));
        cx.global_mut::<PanelKeymaps>()
            .bind("tree", "x", "tree.close");
    });
    cx.simulate_keystrokes("space n");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.simulate_keystrokes("g t");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));

    // Every command can be run by name from the command line.
    cx.simulate_keystrokes(":");
    cx.simulate_input("workspace.prev_tab");
    cx.simulate_keystrokes("enter");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    cx.simulate_keystrokes(":");
    cx.simulate_input("nope");
    cx.simulate_keystrokes("enter");
    assert_eq!(message(cx), "Not an editor command: nope");

    // An engine command on a new key, with the grammar still around it.
    cx.simulate_keystrokes("shift-q w");
    assert_eq!(text(&ws, cx), "two\n");

    // `space` starts the leader bindings, so alone it waits before moving.
    cx.simulate_keystrokes("space");
    assert_eq!(cursor(&ws, cx), (1, 1));
    cx.executor().advance_clock(Duration::from_millis(1100));
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (1, 2));

    // The panels read their keys from a table too.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "to\n");

    // A key bound to a command nobody implements says so.
    cx.update(|_, cx| {
        let keymap = &mut cx.global_mut::<crate::VimGlobal>().0.keymap;
        keymap.bind(MapMode::Normal, "Z", "no.such_command");
    });
    cx.simulate_keystrokes("shift-z");
    assert_eq!(message(cx), "Unknown command: no.such_command");
}

/// Evaluate a form from the command line, and give what the status bar
/// then says: what the form came to, or what went wrong.
fn eval_in(ws: &Entity<Workspace>, source: &str, cx: &mut VisualTestContext) -> String {
    cx.simulate_keystrokes(":");
    cx.simulate_input(&format!("eval {source}"));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    ws.read_with(cx, |ws, _| {
        ws.message.clone().unwrap_or_default().to_string()
    })
}

fn decoration_lines(ws: &Entity<Workspace>, group: &str, cx: &mut VisualTestContext) -> Vec<usize> {
    ws.read_with(cx, |ws, cx| {
        ws.active_editor().unwrap().read(cx).decoration_lines(group)
    })
}

#[gpui::test]
fn programs_read_and_edit_the_file(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one two\nthree\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let eval = |source: &str, cx: &mut VisualTestContext| eval_in(&ws, source, cx);

    // Reading: where the cursor is, and what the text says.
    assert_eq!(eval("(cursor)", cx), "(1 1)");
    assert_eq!(eval("(line-count)", cx), "2");
    assert_eq!(eval(r#"(= (buffer-line 2) "three")"#, cx), "true");
    assert_eq!(eval("(buffer-line 3)", cx), "nil");
    assert_eq!(eval(r#"(= (buffer-text '(1 5) '(1 8)) "two")"#, cx), "true");
    assert_eq!(eval("(length (lines (buffer-text)))", cx), "2");
    assert_eq!(eval("(car (visible-lines))", cx), "1");
    assert_eq!(eval("(mode)", cx), "normal");
    assert_eq!(eval("(selection)", cx), "nil");
    assert_eq!(eval("(diagnostics)", cx), "nil");
    assert_eq!(eval("(all-diagnostics)", cx), "nil");

    // Moving: to a place, or to the nearest there is.
    eval("(set-cursor 2 3)", cx);
    assert_eq!(cursor(&ws, cx), (2, 3));
    eval("(set-cursor 99 99)", cx);
    assert_eq!(cursor(&ws, cx), (2, 5));

    // Editing: each a step to undo, and the cursor follows its text.
    eval("(set-cursor 1 5)", cx);
    eval(r#"(insert "2 ")"#, cx);
    assert_eq!(text(&ws, cx), "one 2 two\nthree\n");
    assert_eq!(cursor(&ws, cx), (1, 7));
    eval(r#"(edit '(1 1) '(1 4) "1")"#, cx);
    assert_eq!(text(&ws, cx), "1 2 two\nthree\n");
    assert_eq!(cursor(&ws, cx), (1, 5));
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one 2 two\nthree\n");
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "one two\nthree\n");
    // Keys, as the keyboard would type them.
    eval(r#"(feed-keys "ggcwONE<Esc>jdd")"#, cx);
    assert_eq!(text(&ws, cx), "ONE two\n");
    assert_eq!(mode(&ws, cx), Mode::Normal);

    // What is selected, asked from a key bound in visual mode.
    lisp(
        r#"(def seen nil)
           (defcommand "user.look" "Note the selection" (fn () (set! seen (list (mode) (selection)))))
           (bind 'visual "Q" "user.look")"#,
        cx,
    );
    cx.simulate_keystrokes("g g v e shift-q");
    cx.run_until_parked();
    assert_eq!(lisp("(car seen)", cx), "visual");
    assert_eq!(lisp(r#"(= (get (nth 1 seen) :text) "ONE")"#, cx), "true");
    assert_eq!(lisp("(get (nth 1 seen) :start)", cx), "(1 1)");
    assert_eq!(lisp("(get (nth 1 seen) :end)", cx), "(1 4)");
    assert_eq!(lisp("(get (nth 1 seen) :kind)", cx), "char");
    cx.simulate_keystrokes("escape ctrl-v l shift-q escape");
    cx.run_until_parked();
    assert_eq!(lisp("(car seen)", cx), "visual-block");
    assert_eq!(lisp("(get (nth 1 seen) :kind)", cx), "block");

    // Mistakes are explained.
    assert_eq!(
        eval("(set-cursor 0 1)", cx),
        "1:1: set-cursor: expected a place, (line column) counted from 1, got (0 1)"
    );
    // With no file there is an empty buffer, which is no file.
    cx.simulate_keystrokes(":");
    cx.simulate_input("bd!");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), None);
    assert_eq!(eval("(current-file)", cx), "nil");
}

#[gpui::test]
fn an_empty_buffer_stands_in_for_no_file(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let (ws, cx) = open(&project, &[], cx);
    let tabs = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.tabs().len())
    };
    assert_eq!(tabs(&ws, cx), 1);
    assert_eq!(active_file(&ws, cx), None);

    // The keys have an editor to go to: `:e` opens a file, which takes
    // the empty buffer's place.
    cx.simulate_keystrokes(": e space a . t x t enter");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    assert_eq!(tabs(&ws, cx), 1);

    // Closing the last file leaves an empty buffer again.
    cx.simulate_keystrokes(":");
    cx.simulate_input("bd");
    cx.simulate_keystrokes("enter");
    assert_eq!(tabs(&ws, cx), 1);
    assert_eq!(active_file(&ws, cx), None);

    // Typed in, it is kept: the next file gets a tab of its own.
    cx.simulate_keystrokes("i h i escape");
    assert_eq!(text(&ws, cx), "hi\n");
    cx.simulate_keystrokes(": e space b . t x t enter");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    assert_eq!(tabs(&ws, cx), 2);
}

#[gpui::test]
fn programs_are_told_what_happens_in_the_editor(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\n"), ("b.txt", "bee\n")]);
    let hooks = r#"
        (def entered nil) (def moves 0) (def changes 0) (def modes nil)
        (add-hook 'buffer-entered (fn (path) (set! entered (cons (path-name path) entered))))
        (add-hook 'cursor-moved (fn () (set! moves (+ moves 1))))
        (add-hook 'buffer-changed (fn () (set! changes (+ changes 1))))
        (add-hook 'mode-changed (fn (mode) (set! modes (cons mode modes))))"#;
    let (ws, cx) = open_with(&[hooks], &project, &["a.txt"], cx);

    // The file the window opened on.
    assert_eq!(lisp("entered", cx), r#"("a.txt")"#);
    assert_eq!(lisp("modes", cx), "(normal)");
    assert_eq!(lisp("(list moves changes)", cx), "(1 0)");

    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert_eq!(lisp("(list moves changes)", cx), "(2 0)");
    // A key that moves nothing tells nothing.
    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert_eq!(lisp("(list moves changes)", cx), "(2 0)");

    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(lisp("(list moves changes)", cx), "(2 1)");
    assert_eq!(text(&ws, cx), "one\nwo\n");

    cx.simulate_keystrokes("i");
    cx.simulate_input("t");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(lisp("modes", cx), "(normal insert normal)");
    assert_eq!(lisp("changes", cx), "2");

    // Another file, and back.
    cx.simulate_keystrokes(":");
    cx.simulate_input("e b.txt");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    assert_eq!(lisp("(car entered)", cx), r#""b.txt""#);
    cx.simulate_keystrokes("g t");
    cx.run_until_parked();
    assert_eq!(lisp("(car entered)", cx), r#""a.txt""#);
    assert_eq!(lisp("(length entered)", cx), "3");

    // A hook there is not.
    assert!(
        eval_in(&ws, "(add-hook 'cursor-move (fn () nil))", cx)
            .starts_with("1:1: cursor-move is not a hook (")
    );
}

#[gpui::test]
fn programs_decorate_the_file(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\nthree\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let eval = |source: &str, cx: &mut VisualTestContext| eval_in(&ws, source, cx);

    let marks = r##"(decorate 'marks (list
        (list :line 1 :sign "!" :color 'warning)
        (list :line 2 :text "the second" :bg "#ff0000")
        (list :start '(3 1) :end '(3 4) :bg 'warning :color 'danger)))"##;
    let marks = marks.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(eval(&marks, cx), "3");
    assert_eq!(decoration_lines(&ws, "marks", cx), [0, 1, 2]);
    // They are drawn without complaint.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    // They stay on their text as lines come and go above it.
    cx.simulate_keystrokes("g g shift-o");
    cx.simulate_input("zero");
    cx.simulate_keystrokes("escape");
    assert_eq!(decoration_lines(&ws, "marks", cx), [1, 2, 3]);
    cx.simulate_keystrokes("j d d");
    assert_eq!(text(&ws, cx), "zero\ntwo\nthree\n");
    assert_eq!(decoration_lines(&ws, "marks", cx), [1, 1, 2]);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    // A group is replaced as a whole, and others are left alone.
    assert_eq!(
        eval("(decorate 'other (list (list :line 3 :sign \"x\")))", cx),
        "1"
    );
    assert_eq!(
        eval("(decorate 'marks (list (list :line 1 :sign \"y\")))", cx),
        "1"
    );
    assert_eq!(decoration_lines(&ws, "marks", cx), [0]);
    assert_eq!(decoration_lines(&ws, "other", cx), [2]);
    assert_eq!(eval("(decorate 'marks nil)", cx), "0");
    assert_eq!(decoration_lines(&ws, "marks", cx), [] as [usize; 0]);

    // Mistakes are explained, and change nothing.
    assert!(
        eval("(decorate 'other (list (list :sign \"x\")))", cx).starts_with(
            "1:1: decorate: expected a decoration with :line, or with :start and :end"
        )
    );
    assert!(
        eval("(decorate 'other (list (list :line 1 :bg 'no-such)))", cx)
            .starts_with("1:1: bg: no-such is not a color of the theme")
    );
    assert_eq!(decoration_lines(&ws, "other", cx), [2]);
}

/// The feed reader of `examples/rss.mlisp`, on feeds that curl reads from
/// the disk rather than from YouTube.
#[gpui::test]
fn the_rss_example_reads_feeds(cx: &mut TestAppContext) {
    let entry = |id: &str, title: &str, date: &str| {
        format!(
            "<entry><title>{title}</title>\
             <link rel=\"alternate\" href=\"https://www.youtube.com/watch?v={id}\"/>\
             <published>{date}T13:49:18+00:00</published>\
             <media:group><media:title>{title}</media:title>\
             <media:description>first line\nsecond &amp; last</media:description>\
             </media:group></entry>"
        )
    };
    let feed = format!(
        "<?xml version=\"1.0\"?><feed><title>The channel</title>{}{}</feed>",
        entry("a1", "Webdev in Jai", "2026-11-14"),
        entry("b2", "C &quot;Strings&quot; &lt;3", "2026-11-05"),
    );
    let project = Project::new(&[("a.txt", "a\n"), ("feed.xml", &feed)]);
    let (_ws, cx) = open_with(&[RSS], &project, &["a.txt"], cx);
    let root = project.0.display();
    lisp(
        &format!(
            "(set! newsticker-feeds (list (list :name \"Local\" :url \"file://{root}/feed.xml\") \
             (list :name \"Gone\" :url \"file://{root}/gone.xml\")))"
        ),
        cx,
    );
    // Hidden: nothing is fetched until it is asked for.
    assert!(!shown("newsticker", cx));
    assert_eq!(lisp("newsticker-fetched", cx), "false");

    cx.simulate_keystrokes(":");
    cx.simulate_input("newsticker");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(shown("newsticker", cx));
    assert!(wait_for(cx, |cx| lisp("newsticker-waiting", cx) == "0"));
    assert_eq!(
        lisp(
            "(map (fn (item) (get item :title)) (newsticker-items-of (first newsticker-feeds)))",
            cx
        ),
        r#"("Webdev in Jai" "C \"Strings\" <3")"#
    );
    assert_eq!(
        lisp("(get (newsticker-current) :date)", cx),
        r#""2026-11-14""#
    );
    assert_eq!(
        lisp("(newsticker-description)", cx),
        r#"("first line" "second & last")"#
    );

    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert_eq!(
        lisp("(get (newsticker-current) :link)", cx),
        r#""https://www.youtube.com/watch?v=b2""#
    );
    // The last item stays the last; another feed starts at its first.
    cx.simulate_keystrokes("j l");
    cx.run_until_parked();
    assert_eq!(lisp("(list newsticker-feed newsticker-item)", cx), "(1 0)");
    assert!(
        lisp(
            "(get (newsticker-news-of (newsticker-current-feed)) :error)",
            cx
        )
        .starts_with("\"curl: exit ")
    );

    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(!shown("newsticker", cx));
}

#[gpui::test]
fn the_todo_example_marks_lines(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n// TODO: two\nthree\n// FIXME four\n")]);
    let (ws, cx) = open_with(&[TODO], &project, &["a.txt"], cx);
    assert_eq!(decoration_lines(&ws, "todo", cx), [3, 1]);
    assert_eq!(lisp("todo-lines", cx), "(2 4)");

    cx.simulate_keystrokes("] shift-t");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes("] shift-t");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (4, 1));
    // Around the end of the file, and back the other way.
    cx.simulate_keystrokes("] shift-t");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes("[ shift-t");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (4, 1));

    // It follows the text as it changes.
    cx.simulate_keystrokes("d d g g shift-o");
    cx.simulate_input("NOTE zero");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "NOTE zero\none\n// TODO: two\nthree\n");
    // Once the typing has paused; until then the signs stay on their text.
    assert_eq!(lisp("todo-lines", cx), "(2 4)");
    assert_eq!(decoration_lines(&ws, "todo", cx), [3, 2]);
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(lisp("todo-lines", cx), "(1 3)");
    assert_eq!(decoration_lines(&ws, "todo", cx), [2, 0]);
}

#[gpui::test]
fn a_markdown_file_has_a_preview_beside_it(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("README.md", "# Title\n\n- one\n"),
        ("docs/README.md", "# Docs\n"),
        ("b.txt", "b\n"),
    ]);
    let (ws, cx) = open_with(&[], &project, &["README.md"], cx);
    let ex = |line: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(":");
        cx.simulate_input(line);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    // Each pane, as what it shows, and whether it has the keys.
    let panes = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            ws.pane_infos(cx)
                .into_iter()
                .map(|pane| {
                    let shows = match (pane.scratch, pane.path) {
                        (Some(name), _) => name.to_string(),
                        (None, Some(path)) => path.file_name().unwrap().to_string_lossy().into(),
                        (None, None) => String::new(),
                    };
                    (shows, pane.focused)
                })
                .collect::<Vec<_>>()
        })
    };

    // A pane split off to the right, the keys staying with the file.
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(
        panes(cx),
        [
            ("README.md".to_string(), true),
            ("README.md (preview)".to_string(), false)
        ]
    );
    assert_eq!(active_file(&ws, cx), Some(project.path("README.md")));

    // Again, to the preview: its text is the file's.
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), None);
    assert_eq!(text(&ws, cx), "# Title\n\n- one\n");
    let preview = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());

    // Back to the file: what is typed there is in the preview at once.
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("README.md")));
    cx.simulate_keystrokes("shift-g o");
    cx.simulate_input("- two");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let shown = |cx: &mut VisualTestContext| preview.read_with(cx, |preview, _| preview.buf.text());
    assert_eq!(shown(cx), "# Title\n\n- one\n- two\n");

    // The same preview again, and nothing is edited in it.
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(panes(cx).len(), 2);
    cx.simulate_keystrokes("j d d");
    cx.run_until_parked();
    assert!(message(&ws, cx).contains("preview"), "{}", message(&ws, cx));
    cx.simulate_keystrokes("i");
    cx.simulate_input("x");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "# Title\n\n- one\n- two\n");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    // It is no file, and has nowhere to be written.
    ex("w", cx);
    assert!(message(&ws, cx).contains("nowhere"), "{}", message(&ws, cx));
    // `:q` closes it, as any tab, and its pane with it.
    ex("q", cx);
    assert_eq!(panes(cx), [("README.md".to_string(), true)]);

    // A file of the same name elsewhere has a preview of its own.
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    ex("e docs/README.md", cx);
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(panes(cx).len(), 3);
    cx.simulate_keystrokes("space m");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "# Docs\n");

    // What is not Markdown has none.
    ex("e b.txt", cx);
    ex("preview", cx);
    assert_eq!(message(&ws, cx), "b.txt is not Markdown");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
}

#[gpui::test]
fn edits_from_a_language_server_reach_every_file(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "let old = 1;\nold + old\n"),
        ("b.txt", "use old;\n"),
    ]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    cx.simulate_keystrokes("j $");
    assert_eq!(cursor(&ws, cx), (2, 9));

    let edit = |line: u32, from: u32, to: u32| lsp_types::TextEdit {
        range: lsp_types::Range::new(
            lsp_types::Position::new(line, from),
            lsp_types::Position::new(line, to),
        ),
        new_text: "renamed".to_string(),
    };
    let uri = |name: &str| crate::lsp::path_to_uri(&project.path(name)).unwrap();
    let changes = [
        (
            uri("a.txt"),
            vec![edit(0, 4, 7), edit(1, 0, 3), edit(1, 6, 9)],
        ),
        (uri("b.txt"), vec![edit(0, 4, 7)]),
    ];
    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes.into_iter().collect()),
        ..Default::default()
    };
    ws.update_in(cx, |ws, window, cx| {
        ws.apply_workspace_edit(edit, window, cx)
    });
    cx.run_until_parked();

    // The open file is edited in its tab, and not written.
    assert_eq!(text(&ws, cx), "let renamed = 1;\nrenamed + renamed\n");
    assert_eq!(project.read("a.txt"), "let old = 1;\nold + old\n");
    // The other on disk.
    assert_eq!(project.read("b.txt"), "use renamed;\n");
    assert_eq!(
        ws.read_with(cx, |ws, _| ws.message.clone()).as_deref(),
        Some("4 change(s) in 2 file(s)")
    );
    // The cursor stayed on its text, and one undo takes it all back.
    assert_eq!(cursor(&ws, cx), (2, 17));
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "let old = 1;\nold + old\n");
}

#[gpui::test]
fn the_new_keys_work_in_a_window(cx: &mut TestAppContext) {
    let source = "\
fn add(left: i32, right: i32) -> i32 {
    left + right
}

fn main() {
    add(1, 2);
}
";
    let project = Project::new(&[("a.rs", source)]);
    let (ws, cx) = open(&project, &["a.rs"], cx);

    // The syntax tree's motions and text objects.
    cx.simulate_keystrokes("] f");
    assert_eq!(cursor(&ws, cx), (5, 1));
    cx.simulate_keystrokes("[ f f l c i a");
    cx.simulate_input("a: u8");
    cx.simulate_keystrokes("escape");
    assert!(text(&ws, cx).starts_with("fn add(a: u8, right: i32) -> i32 {"));
    cx.simulate_keystrokes("u j ] x");
    assert_eq!(mode(&ws, cx), Mode::Visual);
    cx.simulate_keystrokes("escape d a f");
    assert_eq!(text(&ws, cx), "\nfn main() {\n    add(1, 2);\n}\n");
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), source);

    // A block, with something put in front of each of its lines.
    cx.simulate_keystrokes("g g ctrl-v j j shift-i");
    cx.simulate_input("// ");
    cx.simulate_keystrokes("escape");
    assert!(
        text(&ws, cx)
            .starts_with("// fn add(left: i32, right: i32) -> i32 {\n//     left + right\n// }\n")
    );
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), source);

    // Surround, and a macro: quote a word here, then again further down.
    cx.simulate_keystrokes("g g w q a y s i w \" q");
    assert!(text(&ws, cx).starts_with("fn \"add\"(left"));
    cx.simulate_keystrokes("5 j w @ a");
    assert!(text(&ws, cx).contains("    \"add\"(1, 2);"));
    cx.simulate_keystrokes("d s \" u u u");
    assert_eq!(text(&ws, cx), source);

    // Brackets close as they are typed, but a `'` in Rust is a lifetime's.
    cx.simulate_keystrokes("shift-g shift-o");
    cx.simulate_input("fn f<'a>(x: &[");
    assert_eq!(
        ws.read_with(cx, |ws, cx| ws
            .active_editor()
            .unwrap()
            .read(cx)
            .buf
            .line_string(6)),
        "fn f<'a>(x: &[])"
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(eval_in(&ws, "(set-option 'auto-pairs false)", cx), "nil");
    cx.simulate_keystrokes("c c");
    cx.simulate_input("(");
    cx.simulate_keystrokes("escape");
    assert!(text(&ws, cx).ends_with("(\n}\n"));

    // Marks.
    cx.simulate_keystrokes("g g m a shift-g ' a");
    assert_eq!(cursor(&ws, cx), (1, 1));
}

#[gpui::test]
fn chooses_a_code_action_and_shows_a_signature(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "call(1, 2)\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    let uri = crate::lsp::path_to_uri(&project.path("a.txt")).unwrap();

    // Two actions on offer: the second replaces `call` with `sum`.
    let action = |title: &str, name: u32, new_text: &str| {
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, name),
            ),
            new_text: new_text.to_string(),
        };
        lsp_types::CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
            title: title.to_string(),
            edit: Some(lsp_types::WorkspaceEdit {
                changes: Some([(uri.clone(), vec![edit])].into_iter().collect()),
                ..Default::default()
            }),
            ..Default::default()
        })
    };
    let actions = vec![
        action("Call it add", 4, "add"),
        action("Call it sum", 4, "sum"),
    ];
    editor.update(cx, |editor, cx| editor.offer(actions, None, cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.code_action_titles()),
        ["Call it add", "Call it sum"]
    );
    // While the list is up the keys are its own: `j` moves in it.
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "sum(1, 2)\n");
    assert!(editor.read_with(cx, |editor, _| editor.code_action_titles().is_empty()));
    assert_eq!(cursor(&ws, cx), (1, 1));
    // By its number, or not at all.
    let actions = vec![action("Call it add", 3, "add"), action("Call it f", 3, "f")];
    editor.update(cx, |editor, cx| editor.offer(actions.clone(), None, cx));
    cx.simulate_keystrokes("2");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "f(1, 2)\n");
    editor.update(cx, |editor, cx| editor.offer(actions, None, cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(editor.read_with(cx, |editor, _| editor.code_action_titles().is_empty()));
    assert_eq!(text(&ws, cx), "f(1, 2)\n");

    // A signature, with the parameter the cursor is at: named by where it
    // is in the label, or by its text.
    let signature = |parameters: Vec<lsp_types::ParameterLabel>, active: u32| {
        let parameters = parameters
            .into_iter()
            .map(|label| lsp_types::ParameterInformation {
                label,
                documentation: None,
            });
        lsp_types::SignatureHelp {
            signatures: vec![lsp_types::SignatureInformation {
                label: "f(名: i32, b: u8)".to_string(),
                documentation: None,
                parameters: Some(parameters.collect()),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        }
    };
    cx.simulate_keystrokes("i");
    let by_place = vec![
        lsp_types::ParameterLabel::LabelOffsets([2, 8]),
        lsp_types::ParameterLabel::LabelOffsets([10, 15]),
    ];
    editor.update(cx, |editor, cx| {
        editor.offer(Vec::new(), Some(signature(by_place.clone(), 0)), cx)
    });
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let shown = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |editor, _| {
            editor
                .signature_text()
                .map(|(label, active)| (label.to_string(), active.map(str::to_string)))
        })
    };
    assert_eq!(
        shown(cx),
        Some(("f(名: i32, b: u8)".to_string(), Some("名: i32".to_string())))
    );
    editor.update(cx, |editor, cx| {
        editor.offer(Vec::new(), Some(signature(by_place, 1)), cx)
    });
    assert_eq!(shown(cx).unwrap().1.as_deref(), Some("b: u8"));
    let by_text = vec![
        lsp_types::ParameterLabel::Simple("名: i32".to_string()),
        lsp_types::ParameterLabel::Simple("b: u8".to_string()),
    ];
    editor.update(cx, |editor, cx| {
        editor.offer(Vec::new(), Some(signature(by_text, 1)), cx)
    });
    assert_eq!(shown(cx).unwrap().1.as_deref(), Some("b: u8"));
    // It goes when the typing is over.
    cx.simulate_keystrokes("escape");
    assert_eq!(shown(cx), None);
}

#[gpui::test]
fn flash_goes_to_text_on_screen(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one two\nthree two\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let labels = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            let found = editor.vim.flash()?;
            Some(
                found
                    .matches
                    .iter()
                    .filter_map(|m| m.label)
                    .collect::<String>(),
            )
        })
    };
    lisp(
        "(do (def seen nil)
             (add-hook 'keys-pending (fn (pending) (set! seen (cons pending seen)))))",
        cx,
    );

    // `s`, some of the text, and the label of the place: both lines are
    // on screen, and drawn with their labels on.
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    assert_eq!(
        lisp("(car seen)", cx),
        r#"(:tables ("normal") :keys "" :typed "s" :awaiting flash)"#
    );
    cx.simulate_keystrokes("t w");
    cx.run_until_parked();
    assert_eq!(labels(cx).as_deref(), Some("ad"));
    assert_eq!(lisp("(get (car seen) :typed)", cx), r#""stw""#);
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 7));
    assert_eq!(labels(cx), None);
    assert_eq!(lisp("(car seen)", cx), "nil");
    // It was a jump.
    cx.simulate_keystrokes("ctrl-o");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // After an operator it is `z`, `s` being surround's there.
    cx.simulate_keystrokes("d z t h");
    cx.run_until_parked();
    assert_eq!(labels(cx).as_deref(), Some("s"));
    cx.simulate_keystrokes("s");
    assert_eq!(text(&ws, cx), "hree two\n");
    cx.simulate_keystrokes("y s i w )");
    assert_eq!(text(&ws, cx), "(hree) two\n");

    // The key is a binding like any other.
    lisp(
        r#"(do (unbind 'normal "s") (bind 'normal "gs" "motion.flash"))"#,
        cx,
    );
    cx.simulate_keystrokes("g s t w enter");
    assert_eq!(cursor(&ws, cx), (1, 8));
}

#[gpui::test]
fn flash_finds_selects_nodes_and_works_elsewhere(cx: &mut TestAppContext) {
    let source = "fn main() {\n    add(one, two);\n}\n";
    let project = Project::new(&[("a.rs", source)]);
    let (ws, cx) = open(&project, &["a.rs"], cx);
    // What is marked: where each place starts and ends, and its label.
    let marked = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            let view = editor.vim.flash()?;
            let places = view.matches.iter().map(|m| (m.start, m.end, m.label));
            Some((places.collect::<Vec<_>>(), view.dim, view.nodes))
        })
    };

    // `f` looks on the lines below as well, and marks where it could go
    // next; its key goes there, `F` back.
    cx.simulate_keystrokes("f o");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 9));
    assert_eq!(marked(cx), Some((vec![(27, 28, None)], (21, 32), false)));
    cx.simulate_keystrokes("f");
    assert_eq!(cursor(&ws, cx), (2, 16));
    cx.simulate_keystrokes("shift-f");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 9));
    cx.simulate_keystrokes("l h");
    assert_eq!(marked(cx), None);

    // `S`: the syntax nodes around the cursor, from the name out, with
    // their labels drawn beside them.
    cx.simulate_keystrokes("shift-s");
    cx.run_until_parked();
    let (nodes, dim, in_text) = marked(cx).expect("the nodes are labelled");
    assert_eq!((dim, in_text), ((0, 0), true));
    assert_eq!(
        nodes[..3],
        [
            (20, 23, Some('a')),
            (19, 29, Some('s')),
            (16, 29, Some('d'))
        ]
    );
    cx.simulate_keystrokes("s");
    assert_eq!(mode(&ws, cx), Mode::Visual);
    let selected = ws.read_with(cx, |ws, cx| {
        let editor = ws.active_editor().unwrap().read(cx);
        editor.vim.selection(&editor.buf)
    });
    assert_eq!(selected, Some((19, 29)));
    cx.simulate_keystrokes("escape g g");

    // After an operator, the node is what it acts on.
    cx.simulate_keystrokes("f o d shift-s a");
    assert_eq!(text(&ws, cx), "fn main() {\n    add(, two);\n}\n");
    cx.simulate_keystrokes("u g g");

    // `r` after an operator: it goes to work where the text is, and the
    // cursor comes back.
    cx.simulate_keystrokes("d r t w");
    cx.run_until_parked();
    assert_eq!(
        marked(cx).map(|(places, ..)| places),
        Some(vec![(25, 27, Some('a'))])
    );
    cx.simulate_keystrokes("a");
    assert_eq!(cursor(&ws, cx), (2, 14));
    cx.simulate_keystrokes("i w");
    assert_eq!(text(&ws, cx), "fn main() {\n    add(one, );\n}\n");
    assert_eq!(cursor(&ws, cx), (1, 1));
}

fn message(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    workspace.read_with(cx, |ws, _| {
        ws.message.clone().unwrap_or_default().to_string()
    })
}

fn dirty(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    workspace.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).is_dirty())
}

fn tab_titles(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |ws, cx| {
        ws.tabs().iter().map(|tab| tab.title(cx)).collect()
    })
}

#[gpui::test]
fn programs_change_what_is_on_disk(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        (".gitignore", "ignored.txt\n"),
        ("ignored.txt", "i\n"),
    ]);
    let (_, cx) = open(&project, &[], cx);
    let at = |name: &str| format!("\"{}\"", project.path(name).display());

    // Nothing is put in the place of another file.
    let said = lisp(
        &format!("(rename-file {} {})", at("a.txt"), at("b.txt")),
        cx,
    );
    assert!(said.contains("there is something there already"), "{said}");
    assert_eq!(
        (project.read("a.txt"), project.read("b.txt")),
        ("a\n".into(), "b\n".into())
    );
    assert_eq!(
        lisp(
            &format!("(rename-file {} {})", at("a.txt"), at("c.txt")),
            cx
        ),
        "nil"
    );
    assert_eq!(project.read("c.txt"), "a\n");
    assert_eq!(
        lisp(&format!("(file-exists? {})", at("a.txt")), cx),
        "false"
    );

    assert_eq!(lisp(&format!("(make-dir {})", at("d/e")), cx), "nil");
    assert_eq!(lisp(&format!("(dir? {})", at("d/e")), cx), "true");
    assert_eq!(lisp(&format!("(dir? {})", at("c.txt")), cx), "false");
    assert_eq!(lisp(&format!("(delete-file {})", at("d")), cx), "nil");
    assert!(!project.path("d").exists());
    let said = lisp(&format!("(delete-file {})", at("d")), cx);
    assert!(said.contains("there is nothing there"), "{said}");

    // What git ignores is listed only when asked for.
    let names = |all: &str, cx: &mut VisualTestContext| {
        let dir = format!("\"{}\"", project.0.display());
        lisp(
            &format!("(map (fn (e) (get e :name)) (list-dir {dir} {all}))"),
            cx,
        )
    };
    assert_eq!(names("false", cx), r#"(".gitignore" "b.txt" "c.txt")"#);
    assert_eq!(
        names("true", cx),
        r#"(".gitignore" "b.txt" "c.txt" "ignored.txt")"#
    );
}

#[gpui::test]
fn programs_copy_link_and_look_at_files(cx: &mut TestAppContext) {
    let project = Project::new(&[("d/a.txt", "a\n"), ("d/e/b.txt", "b\n"), ("c.txt", "c\n")]);
    let (_, cx) = open(&project, &[], cx);
    let at = |name: &str| format!("\"{}\"", project.path(name).display());
    let info = |name: &str, field: &str, cx: &mut VisualTestContext| {
        lisp(&format!("(get (file-info {}) :{field})", at(name)), cx)
    };

    assert_eq!(lisp(&format!("(file-info {})", at("none")), cx), "nil");
    assert_eq!(info("d/a.txt", "kind", cx), "file");
    assert_eq!(info("d/a.txt", "size", cx), "2");
    assert_eq!(info("d", "kind", cx), "dir");
    assert_eq!(info("d", "dir", cx), "true");

    // Permissions, as a number or as chmod's digits.
    assert_eq!(
        lisp(&format!("(set-file-mode {} \"640\")", at("d/a.txt")), cx),
        "nil"
    );
    assert_eq!(info("d/a.txt", "mode", cx), (0o640).to_string());
    assert_eq!(
        lisp(&format!("(set-file-mode {} 420)", at("d/a.txt")), cx),
        "nil"
    );
    assert_eq!(info("d/a.txt", "mode", cx), (0o644).to_string());

    // A link is itself, and points where it was told to.
    assert_eq!(
        lisp(&format!("(make-link \"a.txt\" {})", at("d/to-a")), cx),
        "nil"
    );
    assert_eq!(info("d/to-a", "kind", cx), "link");
    assert_eq!(info("d/to-a", "target", cx), "\"a.txt\"");
    assert_eq!(info("d/to-a", "dir", cx), "false");
    let said = lisp(&format!("(make-link \"a.txt\" {})", at("d/to-a")), cx);
    assert!(said.contains("exists"), "{said}");
    assert_eq!(
        lisp(
            &format!("(make-link {} {} true)", at("c.txt"), at("hard.txt")),
            cx
        ),
        "nil"
    );
    assert_eq!(info("c.txt", "links", cx), "2");

    // A directory is copied with all in it, links as links, and the times.
    assert_eq!(
        lisp(&format!("(copy-file {} {})", at("d"), at("copy")), cx),
        "nil"
    );
    assert_eq!(project.read("copy/a.txt"), "a\n");
    assert_eq!(project.read("copy/e/b.txt"), "b\n");
    assert_eq!(info("copy/to-a", "target", cx), "\"a.txt\"");
    assert_eq!(
        info("copy/a.txt", "modified", cx),
        info("d/a.txt", "modified", cx)
    );
    let said = lisp(
        &format!("(copy-file {} {})", at("c.txt"), at("d/a.txt")),
        cx,
    );
    assert!(said.contains("there is something there already"), "{said}");
    assert_eq!(project.read("d/a.txt"), "a\n");
    let said = lisp(&format!("(copy-file {} {})", at("d"), at("d/e/d")), cx);
    assert!(said.contains("cannot be copied into itself"), "{said}");
    assert!(!project.path("d/e/d").exists());

    // In the background, and the function is told how it went.
    lisp("(def copied 'waiting)", cx);
    lisp(
        &format!(
            "(copy-file {} {} (fn (problem) (set! copied problem)))",
            at("c.txt"),
            at("later.txt")
        ),
        cx,
    );
    assert!(wait_for(cx, |cx| lisp("copied", cx) != "waiting"));
    assert_eq!(lisp("copied", cx), "nil");
    assert_eq!(project.read("later.txt"), "c\n");

    // Gone for good, a link without what it points to.
    assert_eq!(
        lisp(&format!("(delete-file {} true)", at("copy/to-a")), cx),
        "nil"
    );
    assert!(project.path("copy/a.txt").exists());
    assert_eq!(
        lisp(&format!("(delete-file {} true)", at("copy")), cx),
        "nil"
    );
    assert!(!project.path("copy").exists());
}

#[gpui::test]
fn programs_run_programs_and_match_text(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n")]);
    let (_, cx) = open(&project, &[], cx);

    let dir = format!("\"{}\"", project.0.display());
    lisp("(def ran 'waiting)", cx);
    lisp(
        &format!(
            r#"(spawn "sh" (list "-c" "ls a.txt; echo wrong >&2; exit 3") {dir}
                 (fn (done problem) (set! ran (list done problem))))"#
        ),
        cx,
    );
    assert!(wait_for(cx, |cx| lisp("ran", cx) != "waiting"));
    assert_eq!(
        lisp("ran", cx),
        r#"((:status 3 :out "a.txt\n" :err "wrong\n") nil)"#
    );

    lisp("(set! ran 'waiting)", cx);
    lisp(
        &format!(
            r#"(spawn "no-such-program-at-all" nil {dir}
                 (fn (done problem) (set! ran (list done problem))))"#
        ),
        cx,
    );
    assert!(wait_for(cx, |cx| lisp("ran", cx) != "waiting"));
    assert!(
        lisp("ran", cx).starts_with("(nil \"no-such-program-at-all: "),
        "{}",
        lisp("ran", cx)
    );

    assert_eq!(
        lisp(r#"(regex-match "(\\w+)\\.(rs)?(txt)?$" "dir/main.rs")"#, cx),
        r#"("main.rs" "main" "rs" nil)"#
    );
    assert_eq!(lisp(r#"(regex-match "^x" "main.rs")"#, cx), "nil");
    assert_eq!(
        lisp(
            r#"(regex-replace "(\\w+)\\.txt" "a.txt b.txt" "${1}.md")"#,
            cx
        ),
        r#""a.md b.md""#
    );
    let wrong = cx.update(|_, cx| crate::script::eval(r#"(regex-match "(" "x")"#, cx));
    assert!(wrong.is_err());
}

#[gpui::test]
fn renames_files_by_editing_a_listing(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        ("dir/inner.txt", "inner\n"),
        ("z.txt", "z\n"),
    ]);
    let (ws, cx) = open_with(&[DIRED], &project, &["a.txt"], cx);
    let listing = format!("{}/", project.0.display());
    let editing = format!("(dired-editing? \"{}\")", project.0.display());

    // space - lists the directory of the file, the cursor on its line.
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nb.txt\nz.txt\n");
    assert_eq!(cursor(&ws, cx), (2, 1));
    assert_eq!(tab_titles(&ws, cx), ["a.txt", listing.as_str()]);
    assert_eq!(active_file(&ws, cx), None);

    // i edits the names. A new name renames, one with / moves; the tab of
    // a.txt goes with it, and the listing is not edited any more.
    cx.simulate_keystrokes("i");
    assert_eq!(lisp(&editing, cx), "true");
    cx.simulate_keystrokes("c c");
    cx.simulate_input("c.txt");
    cx.simulate_keystrokes("escape shift-g c c");
    cx.simulate_input("dir/sub/z.txt");
    cx.simulate_keystrokes("escape");
    assert!(dirty(&ws, cx));
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "2 renamed, 0 to the trash");
    assert_eq!(project.read("c.txt"), "a\n");
    assert_eq!(project.read("dir/sub/z.txt"), "z\n");
    assert!(!project.path("a.txt").exists() && !project.path("z.txt").exists());
    assert_eq!(text(&ws, cx), "dir/\nb.txt\nc.txt\n");
    assert!(!dirty(&ws, cx));
    assert_eq!(lisp(&editing, cx), "false");
    assert_eq!(tab_titles(&ws, cx), ["c.txt", listing.as_str()]);

    // What cannot all be done is not done at all, and stays to be fixed.
    cx.simulate_keystrokes("i");
    let refused = |keys: &str, typed: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(keys);
        cx.simulate_input(typed);
        cx.simulate_keystrokes("escape : w enter");
        cx.run_until_parked();
        let said = message(&ws, cx);
        assert!(dirty(&ws, cx), "{said}");
        cx.simulate_keystrokes("u");
        assert_eq!(text(&ws, cx), "dir/\nb.txt\nc.txt\n");
        said
    };
    assert_eq!(
        refused("g g j o", "new.txt", cx),
        "4 lines for 3 files: change names in place, and empty a line to delete"
    );
    assert_eq!(
        refused("g g j c c", "c.txt", cx),
        "c.txt is another line's: rename it first, then this"
    );
    assert_eq!(
        refused("g g j c c", "dir/inner.txt", cx),
        "dir/inner.txt is there already"
    );
    assert!(project.path("b.txt").exists());
    assert_eq!(lisp(&editing, cx), "true");

    // An empty line is a file for the trash, after :w!.
    cx.simulate_keystrokes("g g j c c escape : w enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 to the trash: :w! to do it");
    assert!(project.path("b.txt").exists());
    cx.simulate_keystrokes(": w ! enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "0 renamed, 1 to the trash");
    assert!(!project.path("b.txt").exists());
    assert_eq!(text(&ws, cx), "dir/\nc.txt\n");

    // C-c C-k gives the changes up; C-x C-s writes them, as :w.
    cx.simulate_keystrokes("i g g j c c");
    cx.simulate_input("lost.txt");
    cx.simulate_keystrokes("escape ctrl-c ctrl-k");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\nc.txt\n");
    assert!(!dirty(&ws, cx));
    assert_eq!(lisp(&editing, cx), "false");
    cx.simulate_keystrokes("ctrl-x ctrl-q");
    assert_eq!(lisp(&editing, cx), "true", "{}", message(&ws, cx));
    cx.simulate_keystrokes("c c");
    cx.simulate_input("e.txt");
    cx.simulate_keystrokes("escape ctrl-x ctrl-s");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 renamed, 0 to the trash");
    assert_eq!(text(&ws, cx), "dir/\ne.txt\n");

    // Enter goes into a directory, and - back up to it: keys of the
    // listing's own.
    cx.simulate_keystrokes("g g enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "sub/\ninner.txt\n");
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\ne.txt\n");
    assert_eq!(cursor(&ws, cx), (1, 1));
    // On a file, enter opens it, where - and enter are vim's again.
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("e.txt")));
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("e.txt")));

    // :wq writes what the listing had, though another tab is up by the
    // time the program runs: here e.txt, as many lines long.
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    cx.simulate_keystrokes("i j c c");
    cx.simulate_input("d.txt");
    cx.simulate_keystrokes("escape : w q enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 renamed, 0 to the trash");
    assert_eq!(project.read("d.txt"), "a\n");
    // The listing of dir is still there.
    let inner = format!("{listing}dir/");
    assert_eq!(tab_titles(&ws, cx), ["d.txt", inner.as_str()]);

    // A listing with nothing unsaved is no reason not to quit.
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    cx.simulate_keystrokes(": q a enter");
    cx.run_until_parked();
    assert!(!message(&ws, cx).starts_with("Unsaved"));
}

#[gpui::test]
fn a_listing_keeps_what_is_not_written(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("dir/inner.txt", "inner\n"),
        ("sp ", "s\n"),
    ]);
    let seen = "(def seen nil)
                (add-hook 'keys-pending (fn (p) (when p (set! seen (get p :tables)))))";
    let (ws, cx) = open_with(&[DIRED, seen], &project, &["a.txt"], cx);
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nsp \n");
    // A half-typed command in a listing is looked up in its keys first.
    cx.simulate_keystrokes("g");
    cx.run_until_parked();
    assert_eq!(lisp("seen", cx), r#"("dired" "normal")"#);
    cx.simulate_keystrokes("escape");

    // Not being edited, the listing stays as it is, whatever vim's keys
    // do to it.
    cx.simulate_keystrokes("A");
    cx.run_until_parked();
    assert_eq!(
        message(&ws, cx),
        "The listing is not edited: i edits the names"
    );
    cx.simulate_keystrokes("g U U");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nsp \n");
    assert!(!dirty(&ws, cx));
    assert_eq!(mode(&ws, cx), Mode::Normal);
    // Nor is it typed into.
    cx.simulate_keystrokes("g I");
    cx.run_until_parked();
    assert_eq!(mode(&ws, cx), Mode::Normal);
    cx.simulate_input("zz");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nsp \n");
    // The details and marks are drawn without complaint.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    // Going to another listing and back leaves an edit that is not
    // written, still being edited.
    cx.simulate_keystrokes("g g enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "inner.txt\n");
    cx.simulate_keystrokes("i c c");
    cx.simulate_input("x.txt");
    cx.simulate_keystrokes("escape space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nsp \n");
    // The listing above is not being edited: its keys are dired's.
    cx.simulate_keystrokes("g g enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "x.txt\n");
    assert!(dirty(&ws, cx));
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 renamed, 0 to the trash");
    assert_eq!(project.read("dir/x.txt"), "inner\n");
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (1, 1));
    cx.simulate_keystrokes("j i c c");
    cx.simulate_input("b.txt");
    cx.simulate_keystrokes("escape");

    // A name with a space at its end is left as it is.
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 renamed, 0 to the trash");
    assert_eq!(project.read("b.txt"), "a\n");
    assert_eq!(project.read("sp "), "s\n");
    assert_eq!(text(&ws, cx), "dir/\nb.txt\nsp \n");
}

#[gpui::test]
fn a_listing_marks_files_and_does_things_to_them(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "bb\n"),
        ("c.rs", "c\n"),
        ("dir/inner.txt", "inner\n"),
        ("old~", "o\n"),
    ]);
    let (ws, cx) = open_with(&[DIRED], &project, &["a.txt"], cx);
    let dir = format!("\"{}\"", project.0.display());
    let marked = |mark: &str, cx: &mut VisualTestContext| {
        lisp(
            &format!("(dired-names (dired-marked {dir} \"{mark}\"))"),
            cx,
        )
    };
    let answer = |typed: &str, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        assert!(shown("dired-prompt", cx));
        cx.simulate_input(typed);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    let to = |line: usize, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(&format!("{line} shift-g"));
    };
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nb.txt\nc.rs\nold~\n");

    // m marks and goes on; t turns the marks around; U takes them off.
    cx.simulate_keystrokes("m m");
    assert_eq!(marked("*", cx), r#"("a.txt" "b.txt")"#);
    assert_eq!(cursor(&ws, cx), (4, 1));
    assert!(decoration_lines(&ws, "dired", cx).len() > 5);
    cx.simulate_keystrokes("t");
    assert_eq!(marked("*", cx), r#"("dir" "c.rs" "old~")"#);
    cx.simulate_keystrokes("U");
    assert_eq!(marked("*", cx), "nil");
    cx.simulate_keystrokes("* /");
    assert_eq!(marked("*", cx), r#"("dir")"#);
    cx.simulate_keystrokes("U % m");
    answer("\\.txt$", cx);
    assert_eq!(marked("*", cx), r#"("a.txt" "b.txt")"#);

    // C copies the marked files: into a directory, as there are two.
    cx.simulate_keystrokes("C");
    answer("dir", cx);
    assert!(wait_for(cx, |_| project.path("dir/b.txt").exists()));
    assert_eq!(project.read("dir/a.txt"), "a\n");
    assert!(wait_for(cx, |cx| message(&ws, cx) == "copied 2"));

    // R renames the file on the line, with nothing marked.
    cx.simulate_keystrokes("U");
    to(4, cx);
    cx.simulate_keystrokes("R");
    answer("renamed.rs", cx);
    assert_eq!(message(&ws, cx), "renamed 1");
    assert_eq!(project.read("renamed.rs"), "c\n");
    assert_eq!(text(&ws, cx), "dir/\na.txt\nb.txt\nold~\nrenamed.rs\n");

    // ~ flags the backups, and x deletes the flagged, once told y.
    cx.simulate_keystrokes("~");
    assert_eq!(marked("D", cx), r#"("old~")"#);
    cx.simulate_keystrokes("x");
    answer("n", cx);
    assert!(project.path("old~").exists());
    cx.simulate_keystrokes("x");
    answer("y", cx);
    assert!(!project.path("old~").exists());
    assert_eq!(text(&ws, cx), "dir/\na.txt\nb.txt\nrenamed.rs\n");

    // M sets permissions, S makes a link, + a directory.
    to(2, cx);
    cx.simulate_keystrokes("M");
    answer("600", cx);
    assert_eq!(
        lisp(
            &format!(
                "(get (file-info \"{}\") :mode)",
                project.path("a.txt").display()
            ),
            cx
        ),
        "384"
    );
    cx.simulate_keystrokes("S");
    answer("link.txt", cx);
    assert_eq!(
        lisp(
            &format!(
                "(get (file-info \"{}\") :target)",
                project.path("link.txt").display()
            ),
            cx
        ),
        format!("\"{}\"", project.path("a.txt").display())
    );
    cx.simulate_keystrokes("+");
    answer("made", cx);
    assert!(project.path("made").is_dir());

    // ! runs a shell command on the file, ? standing for it.
    to(
        lisp(&format!("(dired-line-of {dir} \"b.txt\")"), cx)
            .parse()
            .unwrap(),
        cx,
    );
    cx.simulate_keystrokes("!");
    answer("echo ? is here", cx);
    assert!(wait_for(cx, |cx| message(&ws, cx) == "b.txt is here"));

    // D deletes the file on the line.
    cx.simulate_keystrokes("D");
    answer("y", cx);
    assert!(!project.path("b.txt").exists());
    assert!(!shown("dired-prompt", cx));
}

#[gpui::test]
fn a_listing_says_what_ls_says(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n")]);
    let (_, cx) = open_with(&[DIRED], &project, &["a.txt"], cx);
    let lisp = |source: &str, cx: &mut VisualTestContext| lisp(source, cx);

    let mode = |kind: &str, mode: u32, cx: &mut VisualTestContext| {
        lisp(
            &format!("(dired-mode-string (list :kind '{kind} :mode {mode}))"),
            cx,
        )
    };
    assert_eq!(mode("file", 0o644, cx), "\"-rw-r--r--\"");
    assert_eq!(mode("file", 0o4755, cx), "\"-rwsr-xr-x\"");
    assert_eq!(mode("dir", 0o1777, cx), "\"drwxrwxrwt\"");
    assert_eq!(mode("link", 0o2644, cx), "\"lrw-r-Sr--\"");

    let size = |bytes: u64, cx: &mut VisualTestContext| lisp(&format!("(dired-size {bytes})"), cx);
    assert_eq!(size(0, cx), "\"0\"");
    assert_eq!(size(1023, cx), "\"1023\"");
    assert_eq!(size(1536, cx), "\"1.5K\"");
    assert_eq!(size(10240, cx), "\"10K\"");
    assert_eq!(size(1 << 20, cx), "\"1.0M\"");

    let date = |seconds: i64, cx: &mut VisualTestContext| {
        lisp(
            &format!("(do (set! dired-utc-offset 0) (dired-date {seconds}))"),
            cx,
        )
    };
    assert_eq!(date(0, cx), "\"1970-01-01 00:00\"");
    assert_eq!(date(951_868_800, cx), "\"2000-03-01 00:00\"");
    assert_eq!(date(1_709_210_040, cx), "\"2024-02-29 12:34\"");
    assert_eq!(lisp("(dired-offset \"+0800\")", cx), "28800");
    assert_eq!(lisp("(dired-offset \"-0530\")", cx), "-19800");

    // The columns line up, past the longest name.
    lisp(
        "(set! dired-users (list \"501\" \"me\")) (set! dired-groups (list \"20\" \"staff\"))",
        cx,
    );
    assert_eq!(
        lisp(
            "(dired-details (list
               (list :name \"a\" :kind 'file :mode 420 :links 1 :uid 501 :gid 20 :size 2 :modified 0)
               (list :name \"dir\" :kind 'dir :dir true :mode 493 :links 12 :uid 0 :gid 20 :size 384 :modified 0)
               (list :name \"to\" :kind 'link :mode 493 :links 1 :uid 501 :gid 20 :size 1 :modified 0 :target \"a\")))",
            cx
        ),
        concat!(
            "(\"   -rw-r--r--  1 me staff   2 1970-01-01 00:00\" ",
            "\"drwxr-xr-x 12 0  staff 384 1970-01-01 00:00\" ",
            "\"  lrwxr-xr-x  1 me staff   1 1970-01-01 00:00 -> a\")"
        )
    );

    // Owners' names, from ls -lan and ls -la of the same directory.
    lisp("(set! dired-users nil) (set! dired-groups nil)", cx);
    lisp(
        r#"(dired-learn-from "total 8\n-rw-r--r--  1 501  20  2 Nov 15 10:22 a\n::med::\ntotal 8\n-rw-r--r--  1 me  staff  2 Nov 15 10:22 a\n")"#,
        cx,
    );
    assert_eq!(
        lisp("(list dired-users dired-groups)", cx),
        r#"(("501" "me") ("20" "staff"))"#
    );

    // The shell's command line, the names quoted.
    let script = |command: &str, cx: &mut VisualTestContext| {
        lisp(
            &format!("(dired-shell-script \"{command}\" (list \"a b\" \"it's\"))"),
            cx,
        )
    };
    assert_eq!(script("wc -l", cx), r#""wc -l 'a b' 'it'\\''s'""#);
    assert_eq!(
        script("tar czf x.tgz *", cx),
        r#""tar czf x.tgz 'a b' 'it'\\''s'""#
    );
    assert_eq!(
        script("gzip ? -k", cx),
        r#""gzip 'a b' -k; gzip 'it'\\''s' -k""#
    );

    assert_eq!(
        lisp("(dired-sort '(3 1 2 5 4) (fn (a b) (< a b)))", cx),
        "(1 2 3 4 5)"
    );
}

#[gpui::test]
fn programs_give_buffers_keys_of_their_own(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let error = |source: &str, cx: &mut VisualTestContext| match cx
        .update(|_, cx| crate::script::eval(source, cx))
    {
        Ok(value) => panic!("{source}: {value}"),
        Err(error) => error.to_string(),
    };
    assert_eq!(lisp("(defkeys 'mine)", cx), "\"mine\"");
    // Run again, an init file defines it again.
    assert_eq!(lisp("(defkeys 'mine)", cx), "\"mine\"");
    assert!(error("(defkeys 'normal)", cx).contains("a table of keys already"));
    assert!(error("(defkeys 'a.b)", cx).contains("no name for a table"));
    assert!(
        error("(bind 'nope.insert \"x\" \"motion.down\")", cx)
            .contains("a table of defkeys (mine)")
    );

    lisp(
        r#"(do (bind 'mine "x" "motion.down")
               (bind 'mine.insert "<CR>" "insert.escape")
               (defcommand "user.mine" "Have my keys" (fn () (set-buffer-keys '(mine))))
               (defcommand "user.none" "Have none" (fn () (set-buffer-keys nil))))"#,
        cx,
    );
    assert_eq!(
        lisp(
            r#"(map (fn (next) (get next :command)) (bindings 'mine))"#,
            cx
        ),
        r#"("motion.down")"#
    );

    // Until the tab is in the table, x is vim's.
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "ne\ntwo\n");
    cx.simulate_keystrokes(": u s e r . m i n e enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    assert_eq!(cursor(&ws, cx), (2, 1));
    assert_eq!(text(&ws, cx), "ne\ntwo\n");
    cx.simulate_keystrokes("i enter");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert!(lisp("(unbind 'mine \"x\")", cx) == "true");
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "ne\nwo\n");

    cx.simulate_keystrokes(": u s e r . n o n e enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("i enter escape");
    assert_eq!(text(&ws, cx), "ne\n\nwo\n");

    // A panel cannot take the name: its keys would go to the table.
    assert!(
        error("(defpanel 'mine (fn (panel) (col)))", cx).contains("a table of defkeys already")
    );

    // A program's tab written as a file is the program's no longer, keys
    // and all.
    lisp(
        r#"(do (bind 'mine "x" "motion.down")
               (defcommand "user.scratch" "A tab of mine"
                 (fn () (open-scratch "mine" "one\ntwo")
                        (set-buffer-keys '(mine))
                        (message (buffer-keys)))))"#,
        cx,
    );
    cx.simulate_keystrokes(": u s e r . s c r a t c h enter");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), r#"("mine")"#);
    cx.simulate_keystrokes("x");
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes(": w space x . t x t enter");
    cx.run_until_parked();
    assert_eq!(project.read("x.txt"), "one\ntwo\n");
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "one\nwo\n");
}

#[gpui::test]
fn programs_draw_the_status_and_title_bars(cx: &mut TestAppContext) {
    use crate::user_panel::At;
    let project = Project::new(&[("a.txt", "one\ntwo\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let eval = |source: &str, cx: &mut VisualTestContext| {
        let value = lisp(source, cx);
        cx.run_until_parked();
        value
    };
    let click = |x: f32, y: f32, cx: &mut VisualTestContext| {
        cx.simulate_click(point(px(x), px(y)), Modifiers::none());
        cx.run_until_parked();
    };
    let height = cx.update(|window, _| f32::from(window.viewport_size().height));
    let bar = |at: At, cx: &mut VisualTestContext| {
        let ws = workspace_id(cx);
        cx.update(|_, cx| {
            cx.global::<UserPanels>()
                .bar(at, ws)
                .map(|name| name.to_string())
        })
    };

    // A panel at 'status is up once defined, in place of the window's bar,
    // and told what that bar shows. Its click tells what it was drawn with.
    eval("(def seen nil)", cx);
    let layout = r#"(fn (panel)
        (row :width 'full :height 26
             :on-click (fn () (set! seen (list (get panel :mode) (get panel :line)
                                               (get panel :col) (get panel :path)
                                               (get panel :cmdline) (get panel :modified))))
          (text "bar")))"#;
    eval(&format!("(defpanel 'bar :at 'status {layout})"), cx);
    assert_eq!(bar(At::Status, cx), Some("bar".to_string()));
    click(50., height - 13., cx);
    assert_eq!(eval("seen", cx), r#"(normal 1 1 "a.txt" nil false)"#);
    // It is drawn again as the editor changes.
    cx.simulate_keystrokes("j l x");
    click(50., height - 13., cx);
    assert_eq!(eval("seen", cx), r#"(normal 2 2 "a.txt" nil true)"#);
    // The command line is the bar's to show while it is typed.
    cx.simulate_keystrokes(":");
    cx.simulate_input("ab");
    cx.run_until_parked();
    click(50., height - 13., cx);
    assert_eq!(eval("seen", cx), r#"(command 2 2 "a.txt" ":ab" true)"#);
    cx.simulate_keystrokes("escape");

    // A layout that fails gives way to the window's own bar, which says
    // why, until the panel has a layout again.
    eval("(set! seen nil)", cx);
    eval("(set-layout 'bar (fn (panel) (car 5)))", cx);
    assert_eq!(bar(At::Status, cx), None);
    let message = ws.read_with(cx, |ws, _| ws.message.clone().unwrap_or_default());
    assert!(message.starts_with("bar: "), "{message}");
    click(50., height - 13., cx);
    assert_eq!(eval("seen", cx), "nil");
    eval(&format!("(set-layout 'bar {layout})"), cx);
    click(50., height - 13., cx);
    assert_eq!(eval("(car seen)", cx), "normal");
    // Put away, the window's bar is back.
    eval("(hide-panel 'bar)", cx);
    assert_eq!(bar(At::Status, cx), None);
    eval("(set! seen nil)", cx);
    click(50., height - 13., cx);
    assert_eq!(eval("seen", cx), "nil");

    // A panel at 'title is drawn in the title bar, after the room the
    // window's buttons take.
    eval(
        r#"(defpanel 'top :at 'title
             (fn (panel)
               (row :width 'full :height 'full
                    :on-click (fn () (set! seen (get panel :title)))
                 (text "top"))))"#,
        cx,
    );
    assert_eq!(bar(At::Title, cx), Some("top".to_string()));
    click(300., TITLE / 2., cx);
    assert_eq!(eval("seen", cx), r#""a.txt — med""#);

    // The examples draw both bars without failing.
    for program in [STATUSLINE, TITLEBAR] {
        eval(program, cx);
    }
    cx.simulate_keystrokes(":");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(bar(At::Status, cx), Some("statusline".to_string()));
    assert_eq!(bar(At::Title, cx), Some("titlebar".to_string()));
}

#[gpui::test]
fn help_is_made_from_the_commands_and_their_keys(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let mine = r#"
        (defcommand "user.hello" "Say hello" (fn () nil))
        (bind 'normal "<Space>h" "user.hello")
        (defkeys 'listing)
        (bind 'listing.insert "<C-h>" "user.hello")
        (bind 'tree "H" "user.hello")
        (bind 'app "cmd-shift-h" "user.hello")
    "#;
    let (ws, cx) = open_with(&[TREE, mine], &project, &["a.txt"], cx);
    let title = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).title())
    };

    // A command by an ex name: what it says, its names, every key bound.
    cx.simulate_keystrokes(": h space w enter");
    cx.run_until_parked();
    assert_eq!(title(&ws, cx), crate::help::TAB);
    let page = text(&ws, cx);
    assert!(page.starts_with("workspace.save\nWrite the file"), "{page}");
    assert!(page.contains(":w  :write  :workspace.save"), "{page}");
    assert!(page.contains("normal  <Space>w"), "{page}");
    assert!(page.contains("app     cmd-s"), "{page}");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // One of the init file's, in every kind of table; the same tab.
    cx.simulate_keystrokes("G : h e l p space : u s e r . h e l l o enter");
    cx.run_until_parked();
    let page = text(&ws, cx);
    assert!(page.starts_with("user.hello\nSay hello\n"), "{page}");
    for line in [
        "  normal          <Space>h",
        "  listing.insert  <C-h>",
        "  tree            H",
        "  app             cmd-shift-h",
    ] {
        assert!(page.contains(line), "{line:?} in\n{page}");
    }
    assert_eq!(cursor(&ws, cx), (1, 1));
    let tabs = ws.read_with(cx, |ws, _| ws.tabs().len());
    assert_eq!(tabs, 2);

    // The engine's commands have keys, and no command line.
    cx.simulate_keystrokes(": h space m o t i o n . w o r d _ f o r w a r d enter");
    cx.run_until_parked();
    let page = text(&ws, cx);
    assert!(page.contains("the command line cannot"), "{page}");
    assert!(!page.contains("Command line"), "{page}");
    assert!(page.contains("normal    w"), "{page}");

    // With nothing, every command.
    cx.simulate_keystrokes(": h enter");
    cx.run_until_parked();
    let page = text(&ws, cx);
    assert!(page.contains("\nuser.hello "), "{page}");
    assert!(page.contains("Say hello"), "{page}");
    assert!(page.contains("\nworkspace.help "), "{page}");
    let word = page
        .lines()
        .find(|line| line.starts_with("motion.word_forward "))
        .unwrap();
    assert!(word.ends_with(" w"), "{word}");

    // And what is no command says so, and leaves the page alone.
    cx.simulate_keystrokes(": h space n o p e enter");
    cx.run_until_parked();
    let said = ws.read_with(cx, |ws, _| ws.message.clone());
    assert_eq!(said.as_deref(), Some("No help for nope"));
    assert_eq!(text(&ws, cx), page);
}

#[gpui::test]
fn help_works_with_no_keys_bound(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let (ws, cx) = open_window(false, &[], &project, &["a.txt"], cx);
    cx.simulate_keystrokes(": h e l p space w q enter");
    cx.run_until_parked();
    let page = text(&ws, cx);
    assert!(page.starts_with("workspace.save_and_quit\n"), "{page}");
    assert!(
        page.contains(":x  :wq  :xit  :workspace.save_and_quit"),
        "{page}"
    );
    assert!(
        page.contains("  none: (bind 'normal \"<keys>\" \"workspace.save_and_quit\")"),
        "{page}"
    );
}

#[gpui::test]
fn a_listing_runs_every_command(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        ("c.tar.gz", "not really\n"),
        ("d.gz.txt", "d\n"),
        ("run.sh", "echo\n"),
        ("sub/inner.txt", "inner\n"),
        ("sub2/x", "x\n"),
    ]);
    let (ws, cx) = open_with(&[DIRED], &project, &["a.txt"], cx);
    let dir = format!("\"{}\"", project.0.display());
    let marked =
        |cx: &mut VisualTestContext| lisp(&format!("(dired-names (dired-marked {dir} \"*\"))"), cx);
    let answer = |typed: &str, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        assert!(shown("dired-prompt", cx));
        cx.simulate_input(typed);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    let line_of = |name: &str, cx: &mut VisualTestContext| {
        lisp(&format!("(dired-line-of {dir} \"{name}\")"), cx)
    };
    let to = |name: &str, cx: &mut VisualTestContext| {
        let line = line_of(name, cx);
        cx.simulate_keystrokes(&format!("{line} shift-g"));
    };
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(
        text(&ws, cx),
        "sub/\nsub2/\na.txt\nb.txt\nc.tar.gz\nd.gz.txt\nrun.sh\n"
    );

    // > and < go from directory to directory.
    cx.simulate_keystrokes("g g >");
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes(">");
    assert_eq!(message(&ws, cx), "No more directories");
    cx.simulate_keystrokes("<");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // * . marks by an extension, dots and all.
    cx.simulate_keystrokes("* .");
    answer("tar.gz", cx);
    assert_eq!(marked(cx), r#"("c.tar.gz")"#);
    cx.simulate_keystrokes("U * .");
    answer(".txt", cx);
    assert_eq!(marked(cx), r#"("a.txt" "b.txt" "d.gz.txt")"#);

    // % R and % C ask twice: the expression, then its replacement.
    cx.simulate_keystrokes("U");
    to("a.txt", cx);
    cx.simulate_keystrokes("m m % R");
    answer("\\.txt$", cx);
    answer(".md", cx);
    assert_eq!(message(&ws, cx), "renamed 2");
    assert_eq!(project.read("a.md"), "a\n");
    assert_eq!(project.read("b.md"), "b\n");
    assert_eq!(marked(cx), "nil");
    to("a.md", cx);
    cx.simulate_keystrokes("% C");
    answer("^a", cx);
    answer("copy-of-a", cx);
    assert!(wait_for(cx, |_| project.path("copy-of-a.md").exists()));

    // M with a mode chmod knows by letters, T and Z run programs.
    to("run.sh", cx);
    cx.simulate_keystrokes("M");
    answer("u+x", cx);
    assert!(wait_for(cx, |cx| message(&ws, cx) == "changed 1"));
    let mode = lisp(
        &format!(
            "(get (file-info \"{}\") :mode)",
            project.path("run.sh").display()
        ),
        cx,
    );
    assert_eq!(mode.parse::<u32>().unwrap() & 0o100, 0o100);
    cx.simulate_keystrokes("T");
    assert!(wait_for(cx, |cx| message(&ws, cx) == "touched 1"));
    to("d.gz.txt", cx);
    cx.simulate_keystrokes("Z");
    assert!(wait_for(cx, |cx| message(&ws, cx) == "compressed 1"));
    assert!(project.path("d.gz.txt.gz").exists());
    to("d.gz.txt.gz", cx);
    cx.simulate_keystrokes("Z");
    assert!(wait_for(cx, |cx| message(&ws, cx) == "compressed 1"));
    assert_eq!(project.read("d.gz.txt"), "d\n");

    // = diffs the file with another, in a tab of its own.
    to("b.md", cx);
    cx.simulate_keystrokes("=");
    answer("a.md", cx);
    assert!(wait_for(cx, |cx| tab_titles(&ws, cx).contains(&"*diff*".to_string())));
    assert!(text(&ws, cx).contains("-a\n+b\n"), "{}", text(&ws, cx));
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();

    // s lists the newest first, and back; ( hides the details.
    let order =
        |cx: &mut VisualTestContext| lisp(&format!("(dired-names (dired-get {dir} :entries))"), cx);
    let by_name = order(cx);
    cx.simulate_keystrokes("s");
    assert_eq!(message(&ws, cx), "Newest first");
    assert_eq!(lisp(&format!("(dired-get {dir} :sort)"), cx), "time");
    cx.simulate_keystrokes("s");
    assert_eq!(order(cx), by_name);
    let lines = decoration_lines(&ws, "dired", cx).len();
    cx.simulate_keystrokes("(");
    assert!(decoration_lines(&ws, "dired", cx).len() < lines);
    cx.simulate_keystrokes("(");
    assert_eq!(decoration_lines(&ws, "dired", cx).len(), lines);

    // g r reads what changed behind the listing's back.
    std::fs::write(project.path("new.txt"), "n\n").unwrap();
    cx.simulate_keystrokes("g r");
    cx.run_until_parked();
    assert!(text(&ws, cx).contains("new.txt\n"));

    // q closes it.
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(!tab_titles(&ws, cx).iter().any(|title| title.ends_with('/')));
}

#[gpui::test]
fn tab_completes_the_command_line(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/menu.rs", "\n"),
        ("sub/x.txt", "\n"),
        ("my notes/n.txt", "\n"),
        (".hidden", "\n"),
    ]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let line = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let editor = ws.active_editor().unwrap().read(cx);
            editor
                .vim
                .cmdline()
                .map(|(kind, text)| format!("{kind}{text}"))
        })
    };
    let typed = |text: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes("escape :");
        cx.simulate_input(text);
    };

    // Command names: one match is all there is.
    typed("wr", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":write"));

    // Several: each `Tab` the next, then back to what was typed;
    // `S-Tab` the other way.
    typed("q", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":q"));
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":qa"));
    cx.simulate_keystrokes("shift-tab shift-tab");
    assert_eq!(line(cx).as_deref(), Some(":q"));
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(line(cx).as_deref(), Some(":quit"));

    // Typing in between starts again from what is there now.
    typed("q", cx);
    cx.simulate_keystrokes("tab tab");
    cx.simulate_input("l");
    assert_eq!(line(cx).as_deref(), Some(":qal"));
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":qall"));

    // A new command line starts afresh, though it reads like the last
    // one did.
    typed("e s", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e src/"));
    typed("e src/", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e src/main.rs"));

    // The engine's own commands, after a range.
    typed("%nohls", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":%nohlsearch"));

    // Files, from the project's root; into a directory with the next `Tab`;
    // no dot files unless asked for.
    typed("e s", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e src/"));
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e sub/"));
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e s"));
    typed("e src/", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e src/main.rs"));
    typed("w! ", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":w! a.txt"));
    typed("e .h", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e .hidden"));
    typed("e my n", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":e my notes/"));
    let absolute = format!("e {}/sub/", project.0.display());
    typed(&absolute, cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx), Some(format!(":{absolute}x.txt")));

    // Help topics are commands, the engine's too.
    typed("h motion.word_f", cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some(":h motion.word_forward"));

    // Nothing to complete, or an argument nothing completes: no change,
    // and no tab typed.
    for text in ["zzz", "rename fo", "e nothing/"] {
        typed(text, cx);
        cx.simulate_keystrokes("tab");
        assert_eq!(line(cx), Some(format!(":{text}")));
    }

    // A search is not completed.
    cx.simulate_keystrokes("escape /");
    cx.simulate_input("fo");
    cx.simulate_keystrokes("tab");
    assert_eq!(line(cx).as_deref(), Some("/fo\t"));

    // And what was completed runs.
    cx.simulate_keystrokes("escape");
    typed("e sub/", cx);
    cx.simulate_keystrokes("tab enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("sub/x.txt")));
}

/// What the panes show, from the top left: each's file name or scratch
/// name ("" for an empty buffer), the focused one with a * after it.
fn panes(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |ws, cx| {
        let infos = ws.pane_infos(cx);
        infos
            .into_iter()
            .map(|info| {
                let name = match (info.scratch, info.path) {
                    (Some(name), _) => name.to_string(),
                    (None, Some(path)) => path.file_name().unwrap().to_string_lossy().into_owned(),
                    (None, None) => String::new(),
                };
                format!("{name}{}", if info.focused { "*" } else { "" })
            })
            .collect()
    })
}

/// Evaluate Lisp for the workspace, as `:eval` does, and give what it
/// comes to.
fn eval_here(workspace: &Entity<Workspace>, source: &str, cx: &mut VisualTestContext) -> String {
    let source = source.to_string();
    workspace.update_in(cx, |_, window, cx| {
        crate::script::eval_from(source, window, cx)
    });
    cx.run_until_parked();
    message(workspace, cx)
}

#[gpui::test]
fn splits_into_panes_and_moves_between_them(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let (ws, cx) = open(&project, &["a.txt", "b.txt"], cx);
    assert_eq!(panes(&ws, cx), ["a.txt*"]);

    // A new pane, to the right, with an empty buffer and the keys.
    cx.simulate_keystrokes("ctrl-w v");
    assert_eq!(panes(&ws, cx), ["a.txt", "*"]);
    assert_eq!(active_file(&ws, cx), None);

    // A file open in another pane comes over: one buffer to a file. The
    // pane it left shows the tab it has left.
    cx.simulate_keystrokes(": e space a . t x t enter");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt*"]);
    assert_eq!(tab_titles(&ws, cx), ["a.txt"]);

    // Below the right one, then about by direction: across the column,
    // in at its top.
    cx.simulate_keystrokes("ctrl-w s");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt", "*"]);
    cx.simulate_keystrokes("ctrl-w k");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt*", ""]);
    cx.simulate_keystrokes("ctrl-w h");
    assert_eq!(panes(&ws, cx), ["b.txt*", "a.txt", ""]);
    cx.simulate_keystrokes("ctrl-w h");
    assert_eq!(panes(&ws, cx), ["b.txt*", "a.txt", ""]);
    cx.simulate_keystrokes("ctrl-w l ctrl-w j");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt", "*"]);
    cx.simulate_keystrokes("ctrl-w w");
    assert_eq!(panes(&ws, cx), ["b.txt*", "a.txt", ""]);
    cx.simulate_keystrokes("ctrl-w shift-w");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt", "*"]);

    // :q on the last tab of a pane closes the pane, not med; the keys go
    // to the pane before it.
    cx.simulate_keystrokes(": q enter");
    assert_eq!(panes(&ws, cx), ["b.txt", "a.txt*"]);

    // A pane closed keeps its tabs, in the pane that gets the keys.
    cx.simulate_keystrokes(": e space b . t x t enter");
    assert_eq!(panes(&ws, cx), ["", "b.txt*"]);
    cx.simulate_keystrokes("ctrl-w h ctrl-w c");
    assert_eq!(panes(&ws, cx), ["b.txt*"]);
    assert_eq!(tab_titles(&ws, cx), ["a.txt", "b.txt"]);
    cx.simulate_keystrokes("ctrl-w c");
    assert_eq!(panes(&ws, cx), ["b.txt*"]);
    assert!(
        message(&ws, cx).contains("last pane"),
        "{}",
        message(&ws, cx)
    );

    // :only takes the others' tabs in, from the top left.
    cx.simulate_keystrokes(": v s enter : e space a . t x t enter");
    cx.simulate_keystrokes(": s p enter : e space b . t x t enter");
    assert_eq!(panes(&ws, cx), ["", "a.txt", "b.txt*"]);
    cx.simulate_keystrokes(": o n l y enter");
    assert_eq!(panes(&ws, cx), ["b.txt*"]);
    assert_eq!(tab_titles(&ws, cx), ["b.txt", "a.txt"]);
}

#[gpui::test]
fn an_editor_of_another_pane_that_gets_keys_is_the_focused_one(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\ntwo\n"), ("b.txt", "three\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    cx.simulate_keystrokes("ctrl-w v : e space b . t x t enter");
    assert_eq!(panes(&ws, cx), ["a.txt", "b.txt*"]);

    // As a click in it would: the keyboard goes to the left editor.
    let left = ws.read_with(cx, |ws, cx| {
        let infos = ws.pane_infos(cx);
        assert!(!infos[0].focused);
        ws.find_editor(&project.path("a.txt"), cx).unwrap()
    });
    cx.update(|window, cx| gpui::Focusable::focus_handle(left.read(cx), cx).focus(window, cx));
    cx.simulate_keystrokes("j");
    assert_eq!(panes(&ws, cx), ["a.txt*", "b.txt"]);
    assert_eq!(cursor(&ws, cx), (2, 1));
}

#[gpui::test]
fn programs_see_the_panes_and_work_in_them(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    cx.simulate_keystrokes("ctrl-w v : e space b . t x t enter");

    let said = eval_here(&ws, "(map (fn (pane) (get pane :focused)) (panes))", cx);
    assert_eq!(said, "(false true)");

    // The function works in the left pane; the right one keeps the keys.
    let said = eval_here(
        &ws,
        "(with-pane 0 (fn () (open-scratch \"notes\" \"hi\") (scratch-name)))",
        cx,
    );
    assert_eq!(said, "\"notes\"");
    assert_eq!(panes(&ws, cx), ["notes", "b.txt*"]);
    assert_eq!(text(&ws, cx), "two\n");
    let said = eval_here(&ws, "(with-pane 5 (fn () 1))", cx);
    assert!(said.contains("no pane 5"), "{said}");
}

#[gpui::test]
fn dired_offers_the_listing_in_the_next_pane(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("left/a.txt", "a\n"),
        ("left/b.txt", "b\n"),
        ("right/c.txt", "c\n"),
    ]);
    let (ws, cx) = open_with(&[DIRED], &project, &["left/a.txt"], cx);
    let (left, right) = (project.path("left"), project.path("right"));
    let offered = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        assert!(shown("dired-prompt", cx));
        eval_here(&ws, "(input-text 'dired-prompt 'text)", cx)
    };
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "a.txt\nb.txt\n");

    // A listing beside it, in a pane of its own.
    cx.simulate_keystrokes("ctrl-w v");
    eval_here(
        &ws,
        &format!("(dired-show \"{}\" nil)", right.display()),
        cx,
    );
    let (left_tab, right_tab) = (
        format!("{}/", left.display()),
        format!("{}/", right.display()),
    );
    assert_eq!(panes(&ws, cx), [left_tab.clone(), format!("{right_tab}*")]);

    // As in Emacs, by default R offers the listing's own directory.
    cx.simulate_keystrokes("ctrl-w h R");
    assert_eq!(offered(cx), format!("{left_tab:?}"));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // With dired-dwim-target, the other pane's; enter moves the file there,
    // and both listings show it, the keys staying in this one.
    lisp("(set! dired-dwim-target true)", cx);
    cx.simulate_keystrokes("R");
    assert_eq!(offered(cx), format!("{right_tab:?}"));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(project.path("right/a.txt").is_file());
    assert!(!project.path("left/a.txt").exists());
    assert_eq!(text(&ws, cx), "b.txt\n");
    assert_eq!(panes(&ws, cx), [format!("{left_tab}*"), right_tab.clone()]);
    let there = eval_here(&ws, "(with-pane 1 (fn () (lines (buffer-text))))", cx);
    assert_eq!(there, r#"("a.txt" "c.txt")"#);

    // C copies back the same way; with no other listing up, a listing
    // offers its own directory again.
    // The cursor stayed on c.txt there, which is the second line now.
    cx.simulate_keystrokes("ctrl-w l");
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes("g g C");
    assert_eq!(offered(cx), format!("{left_tab:?}"));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(project.read("left/a.txt"), "a\n");
    cx.simulate_keystrokes("ctrl-w o");
    cx.run_until_parked();
    cx.simulate_keystrokes("C");
    assert_eq!(offered(cx), format!("{right_tab:?}"));
}

#[gpui::test]
fn two_listings_side_by_side_by_keys(cx: &mut TestAppContext) {
    let project = Project::new(&[("x.txt", "x\n"), ("sub/y.txt", "y\n")]);
    let (ws, cx) = open_with(&[DIRED, TREE], &project, &["x.txt"], cx);
    lisp("(set! dired-dwim-target true)", cx);
    let root = format!("{}/", project.0.display());
    let sub = format!("{}/", project.path("sub").display());

    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), [format!("{root}*")]);

    // In a new pane, space - has no file to go from: it lists the root,
    // which comes over from the pane it was in. enter goes down from there.
    cx.simulate_keystrokes("ctrl-w v space -");
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), ["x.txt".to_string(), format!("{root}*")]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), ["x.txt".to_string(), format!("{sub}*")]);

    // Back on the left, space - lists x.txt's directory again.
    cx.simulate_keystrokes("ctrl-w h space -");
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), [format!("{root}*"), sub.clone()]);
    cx.simulate_keystrokes("R");
    cx.run_until_parked();
    assert_eq!(
        eval_here(&ws, "(input-text 'dired-prompt 'text)", cx),
        format!("{sub:?}")
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // A click in the other pane gives it the keys, before any key does:
    // a shortcut that is no key of the editor's acts on it too.
    let width = cx.update(|window, _| f32::from(window.viewport_size().width));
    cx.simulate_click(point(px(width - 40.), px(TITLE + 60.)), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), [root.clone(), format!("{sub}*")]);
    // Past the file tree, at the left, which is 260 wide.
    cx.simulate_click(point(px(300.), px(TITLE + 60.)), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(panes(&ws, cx), [format!("{root}*"), sub.clone()]);

    // Reading the disk again goes into the other listing's pane, and the
    // keyboard stays where it was: here in the file tree.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(eval_here(&ws, "(panel-focused? 'tree)", cx), "true");
    fs::write(project.path("sub/z.txt"), "z\n").unwrap();
    ws.update_in(cx, |ws, window, cx| {
        ws.run_command(
            "workspace.sync",
            &crate::commands::Args::default(),
            window,
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(eval_here(&ws, "(panel-focused? 'tree)", cx), "true");
    assert_eq!(panes(&ws, cx), [format!("{root}*"), sub.clone()]);
    let there = eval_here(&ws, "(with-pane 1 (fn () (lines (buffer-text))))", cx);
    assert_eq!(there, r#"("y.txt" "z.txt")"#);
}

/// The terminal shown in the focused pane, if one is.
fn active_terminal(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Option<Entity<crate::terminal::TermView>> {
    workspace.read_with(cx, |ws, _| {
        ws.active_item().and_then(|item| item.terminal().cloned())
    })
}

/// Run until `done` says so of the terminal shown, which is told what
/// its program does from a thread of its own.
fn wait_for_terminal(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    done: impl Fn(&crate::terminal::TermView) -> bool,
) {
    let terminal = active_terminal(workspace, cx).expect("a terminal is shown");
    for _ in 0..500 {
        cx.executor().advance_clock(Duration::from_millis(10));
        cx.run_until_parked();
        if terminal.read_with(cx, |terminal, _| done(terminal)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the terminal never got there");
}

#[gpui::test]
fn a_terminal_runs_in_a_pane_of_its_own(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);

    // A command, in the project's root, in a pane below; the keys go to it.
    cx.simulate_keystrokes(": t e r m i n a l space p w d enter");
    let panes = |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.pane_infos(cx).len());
    assert_eq!(panes(cx), 2);
    assert!(active_terminal(&ws, cx).is_some());
    assert_eq!(active_file(&ws, cx), None, "a terminal is no editor");
    wait_for_terminal(&ws, cx, |terminal| !terminal.is_running());
    let terminal = active_terminal(&ws, cx).unwrap();
    let screen = terminal.read_with(cx, |terminal, _| terminal.screen());
    assert_eq!(screen.line_text(0), project.0.display().to_string());
    assert_eq!(tab_titles(&ws, cx), ["pwd [exited]"]);

    // Done, a key closes it, and its pane with it.
    cx.simulate_keystrokes("enter");
    assert_eq!(panes(cx), 1);
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
}

/// Whether the terminal shown takes the keys itself: not in normal mode.
fn terminal_live(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    workspace.read_with(cx, |ws, _| {
        let item = ws.active_item();
        item.is_some_and(|item| item.terminal().is_some() && item.editor().is_none())
    })
}

#[gpui::test]
fn a_terminal_has_a_normal_mode_as_in_vim(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    cx.simulate_keystrokes(": t e r m i n a l space c a t enter");
    assert!(terminal_live(&ws, cx));

    // Typed text, and keys that are no text, go to the program: `cat`
    // has the line back.
    cx.simulate_input("héllo 中文");
    cx.simulate_keystrokes("enter");
    wait_for_terminal(&ws, cx, |terminal| {
        // Once as the pty echoes it, once as `cat` writes it.
        let screen = terminal.screen();
        let lines = (0..screen.rows).filter(|line| screen.line_text(*line) == "héllo 中文");
        lines.count() == 2
    });

    // `ctrl-\ ctrl-n`: normal mode, in the same tab, in a copy of what
    // went by, the cursor where the terminal's is.
    cx.simulate_keystrokes("ctrl-\\ ctrl-n");
    assert!(active_terminal(&ws, cx).is_some(), "the same tab");
    assert!(!terminal_live(&ws, cx));
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert_eq!(text(&ws, cx), "héllo 中文\nhéllo 中文\n");
    // The empty line the terminal's cursor is on ends the text, as the
    // end of a file does: the cursor is on the line before.
    assert_eq!(cursor(&ws, cx), (2, 1));
    cx.simulate_keystrokes("k");
    assert_eq!(cursor(&ws, cx), (1, 1));
    assert!(!dirty(&ws, cx));

    // `i`: the terminal has the keys again.
    cx.simulate_keystrokes("i");
    assert!(terminal_live(&ws, cx));

    // From normal mode, the keys of a pane: `ctrl-w k` to the file above;
    // `space t`, back to the terminal, out of normal mode.
    cx.simulate_keystrokes("ctrl-\\ ctrl-n ctrl-w k");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    cx.simulate_keystrokes("space t");
    assert!(terminal_live(&ws, cx));
    let panes = ws.read_with(cx, |ws, cx| ws.pane_infos(cx).len());
    assert_eq!(panes, 2, "no new terminal");

    // `ctrl-d` ends `cat`; what it wrote can still be read.
    cx.simulate_keystrokes("ctrl-d");
    wait_for_terminal(&ws, cx, |terminal| !terminal.is_running());
    cx.simulate_keystrokes("ctrl-\\ ctrl-n");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert!(text(&ws, cx).starts_with("héllo 中文\n"));
    // Back to the terminal, a key closes it.
    cx.simulate_keystrokes("i enter");
    let panes = ws.read_with(cx, |ws, cx| ws.pane_infos(cx).len());
    assert_eq!(panes, 1);
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
}

#[gpui::test]
fn programs_drive_terminals(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n"), ("sub/b.txt", "two\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);

    // A command, in a directory of the project, told when it is done.
    let id = eval_here(
        &ws,
        r#"(do (def done nil)
               (add-hook 'terminal-exited (fn (id code) (set! done (list id code))))
               (def t (open-terminal "pwd; read line; echo got $line; exit 3" "sub"))
               (message t))"#,
        cx,
    );
    assert!(id.parse::<u64>().is_ok(), "a number: {id}");
    assert!(terminal_live(&ws, cx));
    let said = eval_here(
        &ws,
        "(message (map (fn (t) (get t :running)) (terminals)))",
        cx,
    );
    assert_eq!(said, "(true)");
    let said = eval_here(
        &ws,
        "(message (map (fn (p) (get p :terminal)) (panes)))",
        cx,
    );
    assert_eq!(said, format!("(nil {id})"));

    // What it is sent, it reads as typed.
    eval_here(&ws, r#"(terminal-send t "hi\r")"#, cx);
    wait_for_terminal(&ws, cx, |terminal| !terminal.is_running());
    cx.run_until_parked();
    assert_eq!(eval_here(&ws, "(message done)", cx), format!("({id} 3)"));
    let text = eval_here(&ws, "(message (terminal-text t))", cx);
    assert!(
        text.contains(&project.path("sub").display().to_string()),
        "{text}"
    );
    assert!(text.contains("got hi"), "{text}");
    let said = eval_here(&ws, "(message (get (first (terminals)) :exit))", cx);
    assert_eq!(said, "3");
    assert_eq!(
        eval_here(&ws, r#"(message (terminal-send t "more"))"#, cx),
        "false"
    );

    // Closed, it is gone, and the keys stay where they were.
    eval_here(&ws, "(focus-editor)", cx);
    eval_here(&ws, "(with-pane 0 (fn () (focus-editor)))", cx);
    assert_eq!(eval_here(&ws, "(message (close-terminal t))", cx), "true");
    assert_eq!(ws.read_with(cx, |ws, cx| ws.pane_infos(cx).len()), 1);
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    assert_eq!(eval_here(&ws, "(message (close-terminal t))", cx), "false");
    let said = eval_here(&ws, r#"(terminal-send t "x")"#, cx);
    assert!(said.contains("there is no terminal"), "{said}");

    // With nothing to run, the terminal still running; keys stop what it
    // runs, and then there is none, so a new one, with the shell.
    let first = eval_here(&ws, "(do (def s (open-terminal \"cat\")) (message s))", cx);
    let again = eval_here(&ws, "(message (open-terminal))", cx);
    assert_eq!(first, again, "the one still running");
    eval_here(&ws, r#"(terminal-keys s "<C-c>")"#, cx);
    let running = |cx: &mut VisualTestContext| {
        eval_here(
            &ws,
            "(message (map (fn (t) (get t :running)) (terminals)))",
            cx,
        )
    };
    for _ in 0..500 {
        cx.executor().advance_clock(Duration::from_millis(10));
        if running(cx) == "(false)" {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(running(cx), "(false)", "ctrl-c stopped cat");
    let shell = eval_here(&ws, "(message (open-terminal))", cx);
    assert_ne!(first, shell);
    assert_eq!(running(cx), "(false true)");
}

/// The `:make` of docs/lisp.md, with a build that fails at once.
#[gpui::test]
fn the_make_of_the_docs_runs(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    let docs = include_str!("../../../docs/lisp.md");
    let example = docs
        .split("```lisp\n(def make-terminal nil)")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the example is in the docs");
    let example = format!("(def make-terminal nil){example}").replace("cargo build", "exit 2");
    let (ws, cx) = open(&project, &["a.txt"], cx);
    eval_here(&ws, &format!("(do {example})"), cx);

    for _ in 0..2 {
        cx.simulate_keystrokes("space b");
        // The keys went back to the file, and the build is below it.
        assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
        assert_eq!(ws.read_with(cx, |ws, cx| ws.pane_infos(cx).len()), 2);
        for _ in 0..500 {
            cx.executor().advance_clock(Duration::from_millis(10));
            cx.run_until_parked();
            if message(&ws, cx) == "The build failed: 2" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(message(&ws, cx), "The build failed: 2");
    }
}

/// `examples/oil.mlisp`: a directory as a buffer to edit, as oil.nvim.
#[gpui::test]
fn edits_directories_as_oil_does(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        (".hidden", "h\n"),
        ("dir/inner.txt", "inner\n"),
    ]);
    let forever = "(set! oil-delete-forever true)";
    let (ws, cx) = open_with(&[OIL, forever], &project, &["a.txt"], cx);
    let root = project.0.display().to_string();
    let listing = format!("oil://{root}/");
    let inner = format!("oil://{root}/dir/");
    let write = |cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(": w enter");
        cx.run_until_parked();
    };
    let asked = |cx: &mut VisualTestContext| {
        lisp(
            "(map (fn (a) (str (oil-verb a) \" \" (get a :label))) (get oil-asking :actions))",
            cx,
        )
    };

    // space - lists the file's directory, the cursor on its line; what
    // starts with a dot waits for g .
    cx.simulate_keystrokes("space -");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\na.txt\nb.txt\n");
    assert_eq!(cursor(&ws, cx), (2, 1));
    assert_eq!(tab_titles(&ws, cx), ["a.txt", listing.as_str()]);
    assert_eq!(active_file(&ws, cx), None);
    cx.simulate_keystrokes("g .");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\n.hidden\na.txt\nb.txt\n");
    assert_eq!(cursor(&ws, cx), (3, 1));
    cx.simulate_keystrokes("g .");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (2, 1));

    // A name changed in place moves its file, a line taken out deletes
    // it, and a line put in makes a file, or a directory: once the popup
    // has said what, and y.
    cx.simulate_keystrokes("c c");
    cx.simulate_input("c.txt");
    cx.simulate_keystrokes("escape j d d o");
    cx.simulate_input("notes.md");
    cx.simulate_keystrokes("escape o");
    cx.simulate_input("sub/");
    cx.simulate_keystrokes("escape");
    assert!(dirty(&ws, cx));
    write(cx);
    assert!(shown("oil-confirm", cx));
    assert_eq!(
        asked(cx),
        r#"("DELETE b.txt" "CREATE notes.md" "CREATE sub/" "MOVE a.txt → c.txt")"#
    );
    cx.simulate_keystrokes("y");
    cx.run_until_parked();
    assert!(!shown("oil-confirm", cx));
    assert_eq!(message(&ws, cx), "2 made, 1 moved, 1 deleted");
    assert_eq!(project.read("c.txt"), "a\n");
    assert_eq!(project.read("notes.md"), "");
    assert!(project.path("sub").is_dir());
    assert!(!project.path("a.txt").exists() && !project.path("b.txt").exists());
    assert_eq!(text(&ws, cx), "dir/\nsub/\nc.txt\nnotes.md\n");
    assert!(!dirty(&ws, cx));
    // The tab of a.txt went with it.
    assert_eq!(tab_titles(&ws, cx), ["c.txt", listing.as_str()]);

    // What cannot all be done is not done at all; n does nothing.
    cx.simulate_keystrokes("g g j c c");
    cx.simulate_input("dir/");
    cx.simulate_keystrokes("escape");
    write(cx);
    assert_eq!(message(&ws, cx), "two lines say dir");
    assert!(!shown("oil-confirm", cx));
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "dir/\nsub/\nc.txt\nnotes.md\n");
    cx.simulate_keystrokes("shift-g d d");
    write(cx);
    assert_eq!(asked(cx), r#"("DELETE notes.md")"#);
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "Nothing done");
    assert!(project.path("notes.md").exists());
    cx.simulate_keystrokes("u");
    assert_eq!(text(&ws, cx), "dir/\nsub/\nc.txt\nnotes.md\n");

    // A line cut here and put in another listing moves its file there, and
    // one copied copies it. The listing cut from keeps its tab, having
    // changes, and the :w in the other writes both.
    cx.simulate_keystrokes("g g j j \" a d d y y g g enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "inner.txt\n");
    assert_eq!(
        tab_titles(&ws, cx),
        ["c.txt", listing.as_str(), inner.as_str()]
    );
    cx.simulate_keystrokes("\" a p p");
    assert_eq!(text(&ws, cx), "inner.txt\nc.txt\nnotes.md\n");
    write(cx);
    assert_eq!(
        asked(cx),
        r#"("COPY notes.md → dir/notes.md" "MOVE c.txt → dir/c.txt")"#
    );
    cx.simulate_keystrokes("y");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 moved, 1 copied");
    assert_eq!(project.read("dir/c.txt"), "a\n");
    assert_eq!(project.read("dir/notes.md"), "");
    assert!(project.path("notes.md").exists() && !project.path("c.txt").exists());
    assert_eq!(text(&ws, cx), "c.txt\ninner.txt\nnotes.md\n");
    assert!(!dirty(&ws, cx));

    // - goes up, the cursor on the directory come from; the listing there
    // had all its changes written, so it is read again.
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\nsub/\nnotes.md\n");
    assert_eq!(cursor(&ws, cx), (1, 1));
    cx.run_until_parked();
    assert!(!dirty(&ws, cx));

    // A listing with changes, not shown while another's :w writes them
    // along with its own, knows only the files of its lines: one made
    // there by the other is not taken for a line taken out.
    cx.simulate_keystrokes("shift-g o");
    cx.simulate_input("keep.txt");
    cx.simulate_keystrokes("escape g g enter");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "c.txt\ninner.txt\nnotes.md\n");
    cx.simulate_keystrokes("c c");
    cx.simulate_input("../moved.txt");
    cx.simulate_keystrokes("escape");
    write(cx);
    assert_eq!(
        asked(cx),
        r#"("CREATE keep.txt" "MOVE dir/c.txt → moved.txt")"#
    );
    cx.simulate_keystrokes("y");
    cx.run_until_parked();
    assert_eq!(message(&ws, cx), "1 made, 1 moved");
    assert_eq!(project.read("moved.txt"), "a\n");
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "dir/\nsub/\nkeep.txt\nmoved.txt\nnotes.md\n");
    assert!(!dirty(&ws, cx));

    // enter on a file opens it in the listing's place; space e lists its
    // directory again, and closes the listing.
    cx.simulate_keystrokes("shift-g enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("notes.md")));
    let titles = tab_titles(&ws, cx);
    assert!(
        !titles.iter().any(|title| title.starts_with("oil://")),
        "{titles:?}"
    );
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(cursor(&ws, cx), (5, 1));
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("notes.md")));
}

#[gpui::test]
fn keeps_the_session_of_a_project(cx: &mut TestAppContext) {
    use crate::session::{Saved, SavedAxis, SavedPane, SavedTab};
    let project = Project::new(&[
        ("a.txt", "one\ntwo\nthree\n"),
        ("b.txt", "b1\nb2\n"),
        ("c.txt", "c\n"),
        ("gone.txt", "x\n"),
    ]);
    let state = project.path(".state");
    cx.update(|cx| {
        crate::init(cx);
        let settings = cx.global_mut::<crate::Settings>();
        settings.lsp = false;
        settings.session_dir = Some(state.clone());
        bind_example_keys(cx);
    });
    let ex = |line: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(":");
        cx.simulate_input(line);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    let session = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.session(cx).map(|session| session.layout))
    };
    let tab = |name: &str, line: usize, col: usize| SavedTab {
        path: project.path(name),
        line,
        col,
    };

    // a.txt (and gone.txt, shown) | b.txt, c.txt (shown) and help, with
    // the keys.
    let root = project.0.clone();
    let (ws, vcx) = cx.add_window_view(|window, cx| Workspace::new(root, Vec::new(), window, cx));
    vcx.run_until_parked();
    ex("e a.txt", vcx);
    vcx.simulate_keystrokes("j j l");
    ex("e gone.txt", vcx);
    ex("vs", vcx);
    ex("e b.txt", vcx);
    vcx.simulate_keystrokes("j");
    ex("e c.txt", vcx);
    ex("h", vcx);
    // A program's text is no file to open again.
    let before = Saved::Split(
        SavedAxis::Row,
        vec![
            Saved::Pane(SavedPane {
                tabs: vec![tab("a.txt", 2, 1), tab("gone.txt", 0, 0)],
                active: 1,
                focused: false,
            }),
            Saved::Pane(SavedPane {
                tabs: vec![tab("b.txt", 1, 0), tab("c.txt", 0, 0)],
                active: 1,
                focused: true,
            }),
        ],
    );
    assert_eq!(session(&ws, vcx), Some(before));
    ex("mks", vcx);
    assert!(
        ws.read_with(vcx, |ws, _| ws.message.clone().unwrap_or_default())
            .starts_with("Session written to"),
    );

    // Started again in the project with no file named: as it was, but
    // for the file that went.
    fs::remove_file(project.path("gone.txt")).unwrap();
    let root = project.0.clone();
    let (ws, vcx) = cx.add_window_view(|window, cx| Workspace::new(root, Vec::new(), window, cx));
    vcx.run_until_parked();
    let after = Saved::Split(
        SavedAxis::Row,
        vec![
            Saved::Pane(SavedPane {
                tabs: vec![tab("a.txt", 2, 1)],
                active: 0,
                focused: false,
            }),
            Saved::Pane(SavedPane {
                tabs: vec![tab("b.txt", 1, 0), tab("c.txt", 0, 0)],
                active: 1,
                focused: true,
            }),
        ],
    );
    assert_eq!(session(&ws, vcx), Some(after));
    assert_eq!(active_file(&ws, vcx), Some(project.path("c.txt")));
    // The panes are the panes, not just a picture of them.
    vcx.simulate_keystrokes("ctrl-w h");
    vcx.run_until_parked();
    assert_eq!(active_file(&ws, vcx), Some(project.path("a.txt")));
    assert_eq!(cursor(&ws, vcx), (3, 2));

    // Named files are opened, not the session.
    let root = project.0.clone();
    let files = vec![project.path("b.txt")];
    let (ws, vcx) = cx.add_window_view(|window, cx| Workspace::new(root, files, window, cx));
    vcx.run_until_parked();
    assert_eq!(
        session(&ws, vcx),
        Some(Saved::Pane(SavedPane {
            tabs: vec![tab("b.txt", 0, 0)],
            active: 0,
            focused: true,
        }))
    );

    // One that cannot be read is said so, and opens nothing.
    let file = crate::session::file(&state, &project.0);
    fs::write(&file, "nope").unwrap();
    let root = project.0.clone();
    let (ws, vcx) = cx.add_window_view(|window, cx| Workspace::new(root, Vec::new(), window, cx));
    vcx.run_until_parked();
    assert_eq!(session(&ws, vcx), None);
    let message = ws.read_with(vcx, |ws, _| ws.message.clone().unwrap_or_default());
    assert!(message.starts_with("Session not restored"), "{message}");
}

/// A language med knows only by name, from the init file: its files are
/// of it, and comment and indent as it says.
#[gpui::test]
fn a_program_names_a_language_and_its_servers(cx: &mut TestAppContext) {
    let project = Project::new(&[("main.dart", "void main() {}\n"), ("a.rs", "fn a() {}\n")]);
    let program = r#"
        (deflanguage "dart" :extensions '("dart") :comment "//" :tab-width 2)
        (lsp-server "dartls" :command "dart" :args '("language-server" "--protocol=lsp")
          :languages '("dart") :settings '(:dart (:lineLength 100)))
        (lsp-server "rust-analyzer" :settings "{\"rust-analyzer\": {}}")
        (lsp-server "pylsp" :enabled false)
    "#;
    let (ws, cx) = open_with(&[program], &project, &["main.dart"], cx);
    assert_eq!(eval_in(&ws, "(buffer-language)", cx), r#""dart""#);
    cx.simulate_keystrokes("g c c");
    assert_eq!(text(&ws, cx), "// void main() {}\n");

    assert_eq!(
        lisp(
            "(map (fn (s) (get s :name)) (filter (fn (s) (get s :enabled)) (lsp-servers)))",
            cx
        )
        .matches("\"pylsp\"")
        .count(),
        0
    );
    assert_eq!(
        lisp(
            "(get (car (filter (fn (s) (= (get s :name) \"dartls\")) (lsp-servers))) :args)",
            cx
        ),
        r#"("language-server" "--protocol=lsp")"#
    );
    let dart = cx.update(|_, cx| crate::lsp_config::servers_for("dart", cx));
    assert_eq!(
        dart[0].settings,
        serde_json::json!({ "dart": { "lineLength": 100 } })
    );

    // A new server needs a command and languages; options are checked.
    for bad in [
        r#"(lsp-server "eslint")"#,
        r#"(lsp-server "eslint" :command "x")"#,
        r#"(lsp-server "rust-analyzer" :settings '(1 2))"#,
        r#"(lsp-server "rust-analyzer" :settings "{oops")"#,
        r#"(lsp-server "rust-analyzer" :colour "red")"#,
        r#"(deflanguage "x" :tab-width 0)"#,
    ] {
        assert!(
            cx.update(|_, cx| crate::script::eval(bad, cx)).is_err(),
            "{bad}"
        );
    }

    // Running the init file again starts from med's own.
    cx.update(|_, cx| crate::lsp_config::reset(cx));
    let dart = cx.update(|_, cx| crate::lsp_config::servers_for("dart", cx));
    assert!(dart.is_empty());
}

#[gpui::test]
fn the_input_method_composes_on_the_command_line(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let project = Project::new(&[("a.txt", "a\nb\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    let cmdline = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |e, _| e.vim.cmdline().map(|(_, t)| t.to_string()))
    };
    let marked = |cx: &mut VisualTestContext| {
        editor.update_in(cx, |e, window, cx| e.marked_text_range(window, cx))
    };

    cx.simulate_keystrokes(":");
    // A wide char already on the line: offsets are UTF-16 units, not bytes.
    editor.update_in(cx, |e, window, cx| {
        e.replace_text_in_range(None, "s/a/你", window, cx)
    });
    assert_eq!(cmdline(cx).as_deref(), Some("s/a/你"));

    // Composing shows on the line but is not on it yet.
    editor.update_in(cx, |e, window, cx| {
        e.replace_and_mark_text_in_range(None, "ㄏ", None, window, cx)
    });
    assert_eq!(cmdline(cx).as_deref(), Some("s/a/你"));
    assert_eq!(marked(cx), Some(5..6));
    let (selected, all) = editor.update_in(cx, |e, window, cx| {
        let selected = e.selected_text_range(false, window, cx).unwrap().range;
        let all = e.text_for_range(0..100, &mut None, window, cx);
        (selected, all)
    });
    assert_eq!(selected, 6..6);
    assert_eq!(all.as_deref(), Some("s/a/你ㄏ"));

    // The input method's window goes where the line's cursor is drawn, at
    // the bottom of the window, not where the buffer's cursor is.
    cx.run_until_parked();
    let (caret, at) = editor.update_in(cx, |e, window, cx| {
        let caret = e.cmdline_caret;
        let at = e.bounds_for_range(5..6, gpui::Bounds::default(), window, cx);
        (caret, at)
    });
    let caret = caret.expect("the status bar draws the line's cursor");
    assert_eq!(at, Some(caret));
    let buffer_cell = editor.read_with(cx, |e, _| e.cell_origin(0)).unwrap();
    assert!(caret.origin.y > buffer_cell.y, "{caret:?} {buffer_cell:?}");

    // Committed, it goes on the line, and runs with it.
    editor.update_in(cx, |e, window, cx| {
        e.replace_text_in_range(None, "好", window, cx)
    });
    assert_eq!(cmdline(cx).as_deref(), Some("s/a/你好"));
    assert_eq!(marked(cx), None);
    cx.simulate_keystrokes("enter");
    assert_eq!(text(&ws, cx), "你好\nb\n");

    // A composition left when the line closes is gone with it.
    cx.simulate_keystrokes(":");
    editor.update_in(cx, |e, window, cx| {
        e.replace_and_mark_text_in_range(None, "ㄋ", None, window, cx)
    });
    cx.simulate_keystrokes("escape");
    assert_eq!(mode(&ws, cx), Mode::Normal);
    assert_eq!(editor.read_with(cx, |e, _| e.cmdline_preedit.clone()), None);
    assert_eq!(marked(cx), None);
}

#[gpui::test]
fn a_program_bar_draws_the_command_lines_cursor(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let project = Project::new(&[("a.txt", "a\nb\n")]);
    let (ws, cx) = open(&project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    let height = cx.update(|window, _| f32::from(window.viewport_size().height));
    let seen = |cx: &mut VisualTestContext| {
        cx.simulate_click(point(px(300.), px(height - 13.)), Modifiers::none());
        cx.run_until_parked();
        lisp("seen", cx)
    };
    lisp("(def seen nil)", cx);
    lisp(
        r#"(defpanel 'bar :at 'status
             (fn (panel)
               (row :width 'full :height 26
                    :on-click (fn () (set! seen (list (get panel :cmdline-before)
                                                      (get panel :cmdline-after)
                                                      (get panel :cmdline-preedit))))
                 (row :width 200 (text (or (get panel :cmdline-before) "")))
                 (caret)
                 (text (or (get panel :cmdline-after) "") :grow true))))"#,
        cx,
    );
    cx.run_until_parked();

    // Closed, there is no line to be on either side of.
    assert_eq!(seen(cx), "(nil nil nil)");

    // The line on each side of its cursor.
    cx.simulate_keystrokes(":");
    cx.simulate_input("ab");
    cx.simulate_keystrokes("left");
    assert_eq!(seen(cx), r#"(":a" "b" nil)"#);

    // What is composed is the bar's to show, at the cursor.
    editor.update_in(cx, |e, window, cx| {
        e.replace_and_mark_text_in_range(None, "ㄏ", None, window, cx)
    });
    cx.run_until_parked();
    assert_eq!(seen(cx), r#"(":a" "b" "ㄏ")"#);

    // The input method's window goes where the bar put `(caret)`.
    let (caret, at) = editor.update_in(cx, |e, window, cx| {
        let caret = e.cmdline_caret;
        let at = e.bounds_for_range(2..3, gpui::Bounds::default(), window, cx);
        (caret, at)
    });
    let caret = caret.expect("the bar draws the line's cursor");
    assert_eq!(at, Some(caret));
    assert_eq!(f32::from(caret.origin.x), 200.);
    assert!(f32::from(caret.origin.y) >= height - 26., "{caret:?}");
    assert_eq!(f32::from(caret.size.width), 0.);

    // Done with the line, the caret is forgotten, and a `(caret)` drawn
    // with no line open does not bring it back.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(editor.read_with(cx, |e, _| e.cmdline_caret), None);
}

#[gpui::test]
fn the_example_status_bar_draws_the_command_line(cx: &mut TestAppContext) {
    use crate::user_panel::At;
    use gpui::EntityInputHandler as _;
    let project = Project::new(&[("a.txt", "a\n")]);
    let (ws, cx) = open_with(&[STATUSLINE], &project, &["a.txt"], cx);
    let editor = ws.read_with(cx, |ws, _| ws.active_editor().unwrap().clone());
    cx.simulate_keystrokes(":");
    cx.simulate_input("ab");
    cx.simulate_keystrokes("left");
    editor.update_in(cx, |e, window, cx| {
        e.replace_and_mark_text_in_range(None, "ㄏ", None, window, cx)
    });
    cx.run_until_parked();
    // Its layout holds, so the bar is still the program's...
    let ws = workspace_id(cx);
    let bar = cx.update(|_, cx| cx.global::<UserPanels>().bar(At::Status, ws).cloned());
    assert_eq!(bar.as_deref(), Some("statusline"));
    // ...and it put the line's cursor where the input method can find it.
    let caret = editor.read_with(cx, |e, _| e.cmdline_caret);
    let height = cx.update(|window, _| f32::from(window.viewport_size().height));
    let caret = caret.expect("the example draws (caret)");
    assert!(f32::from(caret.origin.y) >= height - 26., "{caret:?}");
    assert!(f32::from(caret.origin.x) > 0., "{caret:?}");
}

/// Open a window on each of `projects`, one after the other, as
/// `open-project` does, with the tree loaded: each window's workspace and
/// a context to type in it.
fn open_projects(
    projects: &[&Project],
    list: PathBuf,
    cx: &mut TestAppContext,
) -> Vec<(Entity<Workspace>, VisualTestContext)> {
    cx.update(|cx| {
        crate::init(cx);
        let settings = cx.global_mut::<crate::Settings>();
        settings.lsp = false;
        settings.projects_file = Some(list);
        bind_example_keys(cx);
        crate::script::eval(TREE, cx).unwrap();
    });
    for project in projects {
        cx.update(|cx| crate::open_project(project.0.clone(), cx));
        cx.run_until_parked();
    }
    let all = cx.update(|cx| crate::workspace::all(cx));
    all.into_iter()
        .map(|(window, workspace)| {
            let vcx = VisualTestContext::from_window(window, cx);
            (workspace.upgrade().unwrap(), vcx)
        })
        .collect()
}

#[gpui::test]
fn opens_each_project_in_a_window_of_its_own(cx: &mut TestAppContext) {
    let a = Project::new(&[("a.txt", "a\n")]);
    let b = Project::new(&[("b.txt", "b\n"), ("dir/c.txt", "c\n")]);
    let list = a.path(".state/projects");
    let mut windows = open_projects(&[&a, &b], list.clone(), cx);
    assert_eq!(windows.len(), 2);
    let root = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| ws.root().to_path_buf())
    };
    let (ws_b, mut vcx_b) = windows.pop().unwrap();
    let (ws_a, mut vcx_a) = windows.pop().unwrap();
    assert_eq!(root(&ws_a, &mut vcx_a), a.0);
    assert_eq!(root(&ws_b, &mut vcx_b), b.0);

    // The last opened first.
    let quoted = |project: &Project| format!("{:?}", project.0.display().to_string());
    assert_eq!(
        lisp("(recent-projects)", &mut vcx_a),
        format!("({} {})", quoted(&b), quoted(&a))
    );
    assert_eq!(crate::projects::read(&list), [b.0.clone(), a.0.clone()]);

    // A project with a window already is brought up, not opened again.
    lisp(&format!("(open-project {})", quoted(&a)), &mut vcx_a);
    vcx_a.run_until_parked();
    assert_eq!(vcx_a.update(|_, cx| crate::workspace::all(cx).len()), 2);
    let missing = vcx_a.update(|_, cx| crate::script::eval("(open-project \"/no/such/dir\")", cx));
    assert!(missing.is_err());
}

#[gpui::test]
fn each_window_has_a_tree_of_its_own(cx: &mut TestAppContext) {
    let a = Project::new(&[("a.txt", "a\n")]);
    let b = Project::new(&[("b.txt", "b\n"), ("dir/c.txt", "c\n")]);
    let state = Project::new(&[]);
    let mut windows = open_projects(&[&a, &b], state.path("projects"), cx);
    let (_, mut vcx_b) = windows.pop().unwrap();
    let (_, mut vcx_a) = windows.pop().unwrap();
    let names = |root: &Project, cx: &mut VisualTestContext| {
        let tree = format!("(tree-of {:?})", root.0.display().to_string());
        lisp(
            &format!("(map (fn (e) (get e :name)) (get {tree} :entries))"),
            cx,
        )
    };
    assert_eq!(names(&a, &mut vcx_a), r#"("a.txt")"#);
    assert_eq!(names(&b, &mut vcx_b), r#"("dir" "b.txt")"#);

    // Unfolding in one window leaves the other's tree as it was. The keys
    // go to a window once it is in front, as a click puts it.
    let front = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
    };
    let ex = |line: &str, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes(":");
        cx.simulate_input(line);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    front(&mut vcx_b);
    ex("tree.first", &mut vcx_b);
    ex("tree.expand", &mut vcx_b);
    assert_eq!(names(&b, &mut vcx_b), r#"("dir" "c.txt" "b.txt")"#);
    // So do the panels it puts up.
    ex("eval (show-panel 'tree)", &mut vcx_b);
    assert!(shown("tree", &mut vcx_b));
    assert!(!shown("tree", &mut vcx_a));
    front(&mut vcx_a);
    ex("tree.refresh", &mut vcx_a);
    assert_eq!(names(&a, &mut vcx_a), r#"("a.txt")"#);
    assert_eq!(names(&b, &mut vcx_a), r#"("dir" "c.txt" "b.txt")"#);
}

#[gpui::test]
fn quitting_closes_one_window_or_all(cx: &mut TestAppContext) {
    let a = Project::new(&[("a.txt", "a\n")]);
    let b = Project::new(&[("b.txt", "b\n")]);
    let mut windows = open_projects(&[&a, &b], a.path(".state/projects"), cx);
    let (ws_b, mut vcx_b) = windows.pop().unwrap();
    let (_, mut vcx_a) = windows.pop().unwrap();
    let count = |cx: &mut VisualTestContext| cx.update(|_, cx| crate::workspace::all(cx).len());

    // Unsaved work in any window keeps quitting med (cmd-q) from quitting.
    vcx_b.simulate_keystrokes("i x escape");
    vcx_a.simulate_keystrokes(":");
    vcx_a.simulate_input("workspace.quit_all");
    vcx_a.simulate_keystrokes("enter");
    vcx_a.run_until_parked();
    assert_eq!(count(&mut vcx_a), 2);
    let message = ws_b.read_with(&vcx_b, |ws, _| ws.message.clone().unwrap_or_default());
    assert!(message.contains("Unsaved"), "{message}");
    vcx_b.update(|_, cx| ws_b.update(cx, |ws, _| ws.message = None));

    // :qa closes that window, and only that one: the other's unsaved work
    // is none of its business.
    vcx_a.simulate_keystrokes(": q a enter");
    vcx_a.run_until_parked();
    assert_eq!(count(&mut vcx_b), 1);
    assert_eq!(cx.windows().len(), 1);

    // Its own unsaved work keeps it open, unless :qa!.
    vcx_b.simulate_keystrokes(": q a enter");
    vcx_b.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    let message = ws_b.read_with(&vcx_b, |ws, _| ws.message.clone().unwrap_or_default());
    assert!(message.contains("Unsaved"), "{message}");
}

#[gpui::test]
fn quit_on_the_last_tab_closes_one_window(cx: &mut TestAppContext) {
    let a = Project::new(&[("a.txt", "a\n")]);
    let b = Project::new(&[("b.txt", "b\n")]);
    let mut windows = open_projects(&[&a, &b], a.path(".state/projects"), cx);
    let (_, mut vcx_b) = windows.pop().unwrap();
    let (_, mut vcx_a) = windows.pop().unwrap();
    let count = |cx: &mut VisualTestContext| cx.update(|_, cx| crate::workspace::all(cx).len());

    // :q on the last tab closes that window, and only that one.
    vcx_a.simulate_keystrokes(": q enter");
    vcx_a.run_until_parked();
    assert_eq!(count(&mut vcx_b), 1);
    assert_eq!(cx.windows().len(), 1);
}

/// The empty tab, at the start and after the last one is closed, asks for
/// a command once a key: heard twice, a toggle undid itself.
#[gpui::test]
fn the_empty_tab_runs_a_command_once(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n")]);
    let count = r#"
        (def presses 0)
        (defcommand "test.count" "Count" (fn () (set! presses (+ presses 1))))
        (bind 'normal "<Space>z" "test.count")"#;
    let (_, cx) = open_with(&[count], &project, &[], cx);
    cx.simulate_keystrokes("space z");
    assert_eq!(lisp("presses", cx), "1");
    // The last tab closed, an empty one in its place.
    cx.simulate_keystrokes(": b d enter space z");
    assert_eq!(lisp("presses", cx), "2");
    cx.simulate_keystrokes(": e space a . t x t enter : b d enter space z");
    assert_eq!(lisp("presses", cx), "3");
}

/// The window's own title bar names the project's directory, and a click
/// on it lists the other projects, to switch to one; so does the example's.
#[gpui::test]
fn a_click_on_the_title_switches_project(cx: &mut TestAppContext) {
    let a = Project::new(&[("a.txt", "a\n")]);
    let b = Project::new(&[("b.txt", "b\n")]);
    let state = Project::new(&[]);
    let mut windows = open_projects(&[&b, &a], state.path("projects"), cx);
    let (ws_a, mut vcx) = windows.pop().unwrap();
    let cx = &mut vcx;
    cx.update(|window, _| window.activate_window());
    assert_eq!(ws_a.read_with(cx, |ws, _| ws.root().to_path_buf()), a.0);
    let directory = ws_a.read_with(cx, |ws, _| ws.directory());
    assert!(directory.ends_with(&*a.0.file_name().unwrap().to_string_lossy()));
    lisp(PICKER, cx);
    assert!(!shown("picker", cx));

    // The middle of the bar, where the directory is.
    let width = cx.update(|window, _| f32::from(window.viewport_size().width));
    cx.simulate_click(point(px(width / 2.), px(TITLE / 2.)), Modifiers::none());
    assert!(wait_for(cx, |cx| shown("picker", cx)));
    assert_eq!(lisp("picker-mode", cx), "projects");
    let quoted = format!("{:?}", b.0.display().to_string());
    assert_eq!(lisp("picker-projects", cx), format!("({quoted})"));
    lisp("(hide-panel 'picker)", cx);

    // The example's bar has the directory at the right, as `:directory`.
    let vars = ws_a.read_with(cx, |ws, cx| ws.bar_vars(cx));
    let (_, value) = vars.iter().find(|(name, _)| *name == "directory").unwrap();
    assert_eq!(value.to_string(), format!("{directory:?}"));
    lisp(TITLEBAR, cx);
    cx.run_until_parked();
    assert!(!shown("picker", cx));
    cx.simulate_click(point(px(width - 30.), px(TITLE / 2.)), Modifiers::none());
    assert!(wait_for(cx, |cx| shown("picker", cx)));
    assert_eq!(lisp("picker-mode", cx), "projects");
}
