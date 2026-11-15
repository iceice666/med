//! Headless end-to-end tests: a real `Workspace` in a test window, driven by
//! simulated keystrokes.

use crate::keymap::PanelKeymaps;
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

fn open<'a>(
    project: &Project,
    files: &[&str],
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::init(cx);
        // Language servers wake tasks from their own threads, which the
        // deterministic test executor rejects; tests/lsp.rs covers them.
        cx.global_mut::<crate::Settings>().lsp = false;
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
    let (ws, cx) = open(&project, &["README.md"], cx);

    // Leader key opens the fuzzy finder; typing filters; enter opens.
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.picker.is_some()));
    cx.simulate_input("uhelp");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.picker.is_none()));
    assert_eq!(
        active_file(&ws, cx),
        Some(project.path("src/util/helper.rs"))
    );

    // Escape closes the finder and hands focus back to the editor.
    cx.simulate_keystrokes("space f");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    assert!(ws.read_with(cx, |ws, _| ws.picker.is_none()));
    cx.simulate_keystrokes("g g");
    assert_eq!(cursor(&ws, cx), (1, 1));

    // Project search jumps to the matching line and column.
    cx.simulate_keystrokes("space /");
    cx.run_until_parked();
    cx.simulate_input("needle");
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
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
}

#[gpui::test]
fn navigates_the_file_tree(cx: &mut TestAppContext) {
    let project = Project::new(&[
        ("a.txt", "a\n"),
        ("dir/inner.txt", "inner\n"),
        ("z.txt", "z\n"),
    ]);
    let (ws, cx) = open(&project, &["z.txt"], cx);

    // Directories sort first: dir/, a.txt, z.txt. The open file is revealed.
    cx.simulate_keystrokes("space e");
    cx.run_until_parked();
    cx.simulate_keystrokes("g g l j enter");
    cx.run_until_parked();
    assert_eq!(active_file(&ws, cx), Some(project.path("dir/inner.txt")));

    // Opening a file moved focus to the editor, so vim keys edit again.
    cx.simulate_keystrokes("x");
    assert_eq!(text(&ws, cx), "nner\n");
}

#[gpui::test]
fn draws_the_file_tree_from_its_layout(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let (ws, cx) = open(&project, &[], cx);
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

    // The built-in layout: a 32px header, then 24px rows that open on a
    // click. The second row is b.txt.
    click(32. + 24. + 12., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("b.txt")));

    // A layout of the user's: a button where the header was, taller rows,
    // and directories left out of the picture.
    let layout = r#"
        (set-layout 'tree
          (fn (panel)
            (col :width 'full :height 'full
              (text (str "Find in " (get panel :root-name))
                    :height 40 :on-click "workspace.find_file")
              (rows 'entries :grow true
                (fn (entry)
                  (row :height 30 :width 'full
                       :bg (when (get entry :selected) 'list-active)
                       :on-click (fn ()
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
    // The button runs a command that is not the tree's own.
    assert!(ws.read_with(cx, |ws, _| ws.picker.is_none()));
    click(20., cx);
    assert!(ws.read_with(cx, |ws, _| ws.picker.is_some()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // A layout that fails is drawn as its error, row by row or whole; one
    // that tries to do more than describe is refused; one that never ends
    // is cut short. None of it takes the editor down, and the next layout
    // works.
    for broken in [
        "(set-layout 'tree (fn (panel) (col (rows 'entries (fn (entry) (car 5))))))",
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
        Err("1:1: git is not a panel with a layout (tree)".to_string())
    );
    assert_eq!(
        set_layout("(set-layout 'tree 5)", cx),
        Err("1:1: set-layout: expected a function, got 5".to_string())
    );

    // Running the init file starts from the built-in layouts again.
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.lisp"), cx));
    assert!(!ran);
    cx.run_until_parked();
    // 95px down is the third of the 24px rows under the 32px header; it
    // was the second of the 30px ones.
    click(95., cx);
    assert_eq!(active_file(&ws, cx), Some(project.path("c.txt")));
}

#[gpui::test]
fn scripts_change_keys_and_options(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one two\n"), ("b.txt", "b\n")]);
    let (ws, cx) = open(&project, &["a.txt", "b.txt"], cx);
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
        "1:1: nomal is neither a mode (normal visual operator insert) nor a panel \
         (git git.branches git.files git.stash tree)"
    );
    assert_eq!(
        eval(r#"(bind-keys 'tree "X" "j")"#, cx),
        "1:1: keys stand for other keys only in an editor mode, not in tree"
    );
    assert_eq!(
        eval("(set-option 'font-sise 12)", cx),
        "1:1: font-sise is not an option (font-size font-family lsp)"
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

    // On the left a panel goes between the file tree and the editor, and
    // `:shown` has it up from the start.
    eval(
        r#"(defpanel 'beside :at 'left :width 100 :shown true
             (fn (panel)
               (col :width 'full :height 'full
                 (text "x" :height 30 :on-click (fn () (set! clicks 100))))))"#,
        cx,
    );
    click(260. + 50., 15., cx);
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

    // Mistakes are explained.
    assert_eq!(
        eval("(show-panel 'nope)", cx),
        "1:1: nope is not a panel of yours (counter beside letters ask)"
    );
    assert_eq!(
        eval("(defpanel 'tree (fn (panel) (col)))", cx),
        "1:1: tree is one of the editor's own panels: set-layout changes how it looks"
    );
    assert_eq!(
        eval("(defpanel 'x :at 'top (fn (panel) (col)))", cx),
        "1:1: at: expected 'left, 'right, 'popup, got top"
    );
    assert_eq!(
        eval("(defpanel 'x :colour 1 (fn (panel) (col)))", cx),
        "1:1: colour is not an option of defpanel (at width shown)"
    );
    assert_eq!(
        eval("(defpanel 'x 5)", cx),
        "1:1: defpanel: expected a function, last, got 5"
    );
    assert_eq!(
        eval("(set-layout 'x (fn (panel) (col)))", cx),
        "1:1: x is not a panel with a layout (tree counter beside letters ask)"
    );

    // Running the init file again forgets the panels it did not define
    // this time. One it does define is up if it was up before.
    let ran = cx.update(|_, cx| crate::script::run_init(&project.path("nope.lisp"), cx));
    assert!(!ran);
    cx.run_until_parked();
    assert_eq!(
        eval("(panel-shown? 'counter)", cx),
        "1:1: counter is not a panel of yours ()"
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
fn stages_and_commits_from_the_git_view(cx: &mut TestAppContext) {
    let project = Project::new(&[("a.txt", "one\n")]);
    project.git_init();
    project.git(&["add", "-A"]);
    project.git(&["commit", "-q", "-m", "first"]);
    fs::write(project.path("a.txt"), "one\ntwo\n").unwrap();
    let repo = med_git::Repo::discover(&project.0).unwrap();

    let (ws, cx) = open(&project, &["a.txt"], cx);
    cx.simulate_keystrokes("space g");
    assert!(ws.read_with(cx, |ws, _| ws.show_git));
    let git = ws.read_with(cx, |ws, _| ws.git.clone());
    assert!(wait_for(cx, |cx| git
        .read_with(cx, |git, _| git.selected_file().is_some())));
    assert_eq!(
        git.read_with(cx, |git, _| git.selected_file().unwrap().code()),
        " M"
    );

    // space stages the selected file.
    cx.simulate_keystrokes("space");
    assert!(wait_for(cx, |cx| {
        git.read_with(cx, |git, _| {
            git.selected_file().is_some_and(|f| f.code() == "M ")
        })
    }));
    assert_eq!(repo.status().unwrap()[0].code(), "M ");

    // c opens the message prompt; enter commits.
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    cx.simulate_input("second commit");
    cx.simulate_keystrokes("enter");
    assert!(wait_for(cx, |cx| git
        .read_with(cx, |git, _| git.selected_file().is_none())));
    let log = repo.log(None, 10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "second commit");

    // n in the branches panel creates and checks out a branch.
    cx.simulate_keystrokes("3 n");
    cx.run_until_parked();
    cx.simulate_input("feature");
    cx.simulate_keystrokes("enter");
    assert!(wait_for(cx, |cx| git
        .read_with(cx, |git, _| git.branch_name() == Some("feature"))));
    assert_eq!(repo.head().unwrap().name, "feature");

    // q closes the view and returns to the editor.
    cx.simulate_keystrokes("q");
    assert!(!ws.read_with(cx, |ws, _| ws.show_git));
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
    let (ws, cx) = open(&project, &["a.txt", "b.txt"], cx);
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
