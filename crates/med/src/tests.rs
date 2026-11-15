//! Headless end-to-end tests: a real `Workspace` in a test window, driven by
//! simulated keystrokes.

use crate::workspace::Workspace;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use med_vim::Mode;
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
