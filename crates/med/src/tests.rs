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

const VIM: &str = include_str!("../../../examples/vim.lisp");
const KEYS: &str = include_str!("../../../examples/keys.lisp");
const TREE: &str = include_str!("../../../examples/tree.lisp");
const PICKER: &str = include_str!("../../../examples/picker.lisp");
const GIT: &str = include_str!("../../../examples/git.lisp");
const WHICH_KEY: &str = include_str!("../../../examples/which-key.lisp");
const TODO: &str = include_str!("../../../examples/todo.lisp");

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
    cx.update(|_, cx| cx.global::<UserPanels>().is_shown(panel))
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
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs.len()), 2);

    // gt / gT switch tabs.
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));
    cx.simulate_keystrokes("g t");
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    cx.simulate_keystrokes("g shift-t");
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

    cx.simulate_keystrokes("x : q enter");
    assert_eq!(
        ws.read_with(cx, |ws, _| ws.tabs.len()),
        2,
        ":q must not close a dirty tab"
    );
    cx.simulate_keystrokes(": q ! enter");
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs.len()), 1);
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

    // Directories sort first: dir/, a.txt, z.txt. The open file is revealed.
    assert_eq!(names(cx), r#"("dir" "a.txt" "z.txt")"#);
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
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
    // last row, and space e from the tree is the way back out of it.
    cx.simulate_keystrokes("space e h");
    cx.run_until_parked();
    assert_eq!(lisp("(get (tree-entry) :name)", cx), "\"dir\"");
    cx.simulate_keystrokes("h shift-g");
    cx.run_until_parked();
    assert_eq!(names(cx), r#"("dir" "a.txt" "z.txt")"#);
    assert_eq!(lisp("(get (tree-entry) :name)", cx), "\"z.txt\"");
    cx.simulate_keystrokes("space e x");
    cx.run_until_parked();
    assert_eq!(text(&ws, cx), "ner\n");

    // A file that appears on disk is in the tree once the editor has
    // reason to look: here, a file being written.
    fs::write(project.path("b.txt"), "b\n").unwrap();
    cx.simulate_keystrokes(": w enter");
    cx.run_until_parked();
    assert_eq!(names(cx), r#"("dir" "a.txt" "b.txt" "z.txt")"#);

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
    let click = |y: f32, cx: &mut VisualTestContext| {
        cx.simulate_click(point(px(40.), px(y)), Modifiers::none());
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
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs.len()), 3);
    click(20., cx);
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs.len()), 2);

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
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.lisp"), cx));
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
        "1:1: font-sise is not an option (font-size font-family lsp key-timeout auto-pairs format-on-save)"
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
    click(50., 15., cx);
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
    click(width - 60., 30. + 15., cx);
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
    click(width - 120. - 100., 15., cx);
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
        "1:1: at: expected 'left, 'right, 'bottom, 'popup, 'full, got top"
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
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.lisp"), cx));
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

#[gpui::test]
fn runs_an_init_file(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        (
            "config/init.lisp",
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

    let init = project.path("config/init.lisp");
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
    let missing = project.path("config/nope.lisp");
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
    cx.update(|_, cx| crate::script::run_init(&project.path("nope.lisp"), cx));
    wait(500, cx);
    assert_eq!(lisp("fired", cx), "(quick)");
}

#[gpui::test]
fn programs_load_files_and_hear_of_changes(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        (
            "config/init.lisp",
            "(def seen nil)\n\
             (load \"lib/hooks.lisp\")\n\
             (load \"lib/broken.lisp\")\n",
        ),
        (
            "config/lib/hooks.lisp",
            "(add-hook 'opened (fn () (set! seen (cons 'opened seen))))\n\
             (add-hook 'files-changed (fn () (set! seen (cons 'changed seen))))\n\
             (load \"more.lisp\")\n",
        ),
        ("config/lib/more.lisp", "(def more 'loaded)\n"),
        ("config/lib/broken.lisp", "\n(car 5)\n"),
    ]);
    let init = project.path("config/init.lisp");
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
            project.path("config/lib/broken.lisp").display()
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
         buffer-entered buffer-changed cursor-moved mode-changed diagnostics-changed)"
    );
    assert!(
        error("(load \"/nowhere/at/all.lisp\")", cx).starts_with("1:1: /nowhere/at/all.lisp: ")
    );

    // Running the init file again forgets the hooks it set before.
    cx.update(|_, cx| crate::script::run_init(&project.path("config/nope.lisp"), cx));
    cx.simulate_keystrokes("i z escape : w enter");
    cx.run_until_parked();
    assert_eq!(lisp("seen", cx), "(changed opened)");
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
    assert!(config.join("init.lisp").is_file());
    cx.simulate_keystrokes("j d d");
    assert_eq!(text(&ws, cx), "one\n");

    // A second time, nothing is overwritten unless asked.
    fs::write(config.join("keys.lisp"), "; mine\n").unwrap();
    ws.update_in(cx, |ws, window, cx| {
        ws.install_default_config(&config, false, window, cx)
    });
    let said = ws.read_with(cx, |ws, _| ws.message.clone()).unwrap();
    assert!(said.contains(":install_default_config!"), "{said}");
    assert_eq!(
        fs::read_to_string(config.join("keys.lisp")).unwrap(),
        "; mine\n"
    );
}

#[gpui::test]
fn the_examples_load_together(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let init = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/init.lisp");
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
        ["tree", "picker", "git", "git-prompt", "which-key"]
    );

    // The leader keys work from the tree as they do from an editor.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    assert!(shown("picker", cx));
    cx.simulate_input("b.t");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));
    // Put away without choosing, the picker gives the keyboard back to
    // the tree it was called from.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown("picker", cx));
    cx.simulate_keystrokes("g g enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("a.txt")));

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
        ["tree", "picker", "git", "git-prompt", "which-key"]
    );
    assert_eq!(lisp("(length tree-entries)", cx), "2");
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
        r#"("Go to the file tree, or back from it" nil)"#
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
    assert_eq!(ws.read_with(cx, |ws, _| ws.tabs.len()), 1);
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
    // With no file there is nothing to ask about.
    cx.simulate_keystrokes(":");
    cx.simulate_input("bd!");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.active_editor().is_none()));
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
