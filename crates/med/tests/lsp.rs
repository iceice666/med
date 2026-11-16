//! Language-server integration, against a real clangd.
//!
//! This is its own binary with `harness = false`: the server talks to us
//! from other threads, so it needs GPUI's real executor on the real main
//! thread rather than the deterministic test one.

use gpui::{AppContext as _, AsyncApp, Entity};
use med::commands::{Args, Commands, Handler};
use med::editor::{Editor, EditorEvent};
use med::lsp::LspStore;
use med_vim::Key;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

const SOURCE: &str = "\
int add(int a, int b) {
    return a + b;
}

int main(void) {
    int x = add(1, 2)
    return x;
}
";

fn has_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

fn keys(text: &str) -> Vec<Key> {
    text.chars().map(Key::Char).collect()
}

/// Poll `check` for up to ten seconds.
async fn wait_for(
    what: &str,
    editor: &Entity<Editor>,
    cx: &mut AsyncApp,
    check: impl Fn(&Editor) -> bool,
) -> Result<(), String> {
    for _ in 0..200 {
        if editor.read_with(cx, |editor, _| check(editor)) {
            return Ok(());
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    Err(format!("timed out waiting for {what}"))
}

/// Run one of the editor's commands, as a key bound to it would.
fn run(editor: &Entity<Editor>, cx: &mut AsyncApp, name: &str, arg: &str) {
    editor.update(cx, |editor, cx| {
        let Some(Handler::Editor(run)) = cx.global::<Commands>().handler(name) else {
            panic!("{name} is not a command of the editor's");
        };
        let args = Args {
            arg: arg.to_string(),
            ..Args::default()
        };
        run(editor, &args, cx);
    });
}

async fn scenario(
    editor: Entity<Editor>,
    jumps: Rc<RefCell<Vec<(PathBuf, u32, u32)>>>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    // 1. Diagnostics: the semicolon missing from line 6, which clangd
    //    reports on the token after it, at the start of line 7.
    editor.update(cx, |editor, cx| editor.feed_keys(keys("6j"), cx));
    wait_for("a diagnostic", &editor, cx, |e| {
        e.diagnostic_at_cursor().is_some()
    })
    .await?;
    let message = editor.read_with(cx, |e, _| e.diagnostic_at_cursor().unwrap().message.clone());
    if !message.contains("';'") {
        return Err(format!("unexpected diagnostic: {message}"));
    }
    println!("ok  diagnostic: {message}");

    // 2. Incremental sync: fixing the line clears it.
    editor.update(cx, |editor, cx| {
        editor.feed_keys(keys("kA"), cx);
        editor.type_text(";", cx);
        editor.press(Key::Esc, cx);
        editor.press(Key::Char('j'), cx);
    });
    wait_for("the diagnostic to clear", &editor, cx, |e| {
        e.diagnostic_at_cursor().is_none()
    })
    .await?;
    println!("ok  diagnostic cleared after the edit");

    // 3. Hover on `add` in the call.
    editor.update(cx, |editor, cx| editor.feed_keys(keys("k0faK"), cx));
    wait_for("hover", &editor, cx, |e| {
        e.hover_text().is_some_and(|text| text.contains("add"))
    })
    .await?;
    println!("ok  hover");

    // 4. Go to definition.
    editor.update(cx, |editor, cx| editor.feed_keys(keys("gd"), cx));
    for _ in 0..200 {
        if !jumps.borrow().is_empty() {
            break;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    match jumps.borrow().first() {
        Some((path, 0, 4)) if path.ends_with("main.c") => println!("ok  definition"),
        other => return Err(format!("unexpected definition target: {other:?}")),
    }

    // 5. Completion: a new line calling `ad…` offers `add`, and accepting
    //    it inserts the name.
    editor.update(cx, |editor, cx| {
        editor.press(Key::Char('o'), cx);
        editor.type_text("x = ad", cx);
    });
    wait_for("completions", &editor, cx, |e| {
        e.completion_labels()
            .iter()
            .any(|label| label.contains("add"))
    })
    .await?;
    editor.update(cx, |editor, cx| {
        editor.press(Key::Tab, cx);
        editor.press(Key::Esc, cx);
    });
    let line = editor.read_with(cx, |e, _| e.buf.line_string(6));
    if !line.trim_start().starts_with("x = add") {
        return Err(format!("completion inserted {line:?}"));
    }
    println!("ok  completion: {:?}", line.trim());

    // 6. Signature help: opening the call shows what it takes, and the
    //    parameter follows the arguments as they are typed.
    editor.update(cx, |editor, cx| {
        editor.press(Key::Char('A'), cx);
        editor.type_text("(", cx);
    });
    wait_for("the signature", &editor, cx, |e| {
        e.signature_text()
            .is_some_and(|(label, active)| label.contains("add(") && active == Some("int a"))
    })
    .await?;
    editor.update(cx, |editor, cx| editor.type_text("1, ", cx));
    wait_for("the second parameter", &editor, cx, |e| {
        e.signature_text()
            .is_some_and(|(_, active)| active == Some("int b"))
    })
    .await?;
    editor.update(cx, |editor, cx| {
        editor.type_text("2", cx);
        editor.press(Key::Esc, cx);
        editor.press(Key::Char('A'), cx);
        editor.type_text(";", cx);
        editor.press(Key::Esc, cx);
    });
    let line = editor.read_with(cx, |e, _| e.buf.line_string(6));
    if line.trim() != "x = add(1, 2);" || editor.read_with(cx, |e, _| e.signature_text().is_some())
    {
        return Err(format!("after the call was typed: {line:?}"));
    }
    println!("ok  signature help");

    // 7. Rename: `add` becomes `sum` where it is defined and where it is
    //    called.
    editor.update(cx, |editor, cx| editor.feed_keys(keys("ggw"), cx));
    run(&editor, cx, "editor.rename", "sum");
    wait_for("the rename", &editor, cx, |e| {
        let text = e.buf.text();
        text.contains("int sum(int a") && text.contains("x = sum(1, 2);") && !text.contains("add")
    })
    .await?;
    println!("ok  rename");

    // 8. Format: a line pushed out of place is put back, in one step.
    editor.update(cx, |editor, cx| {
        editor.feed_keys(keys("ggj0i"), cx);
        editor.type_text("        ", cx);
        editor.press(Key::Esc, cx);
    });
    run(&editor, cx, "editor.format", "");
    wait_for("the formatter", &editor, cx, |e| {
        let text = e.buf.text();
        text.contains("return a + b;") && !text.lines().any(|line| line.starts_with("     "))
    })
    .await
    .map_err(|error| format!("{error}: {:?}", editor.read_with(cx, |e, _| e.buf.text())))?;
    let formatted = editor.read_with(cx, |e, _| e.buf.text());
    editor.update(cx, |editor, cx| editor.feed_keys(keys("u"), cx));
    let undone = editor.read_with(cx, |e, _| e.buf.line_string(1));
    if !undone.starts_with("            ") {
        return Err(format!("one undo after formatting left {undone:?}"));
    }
    editor.update(cx, |editor, cx| editor.feed_keys([Key::Ctrl('r')], cx));
    if editor.read_with(cx, |e, _| e.buf.text()) != formatted {
        return Err("redo did not bring the formatted text back".to_string());
    }
    println!("ok  format");

    // 9. Code action: take a semicolon away, and the fix for the error
    //    that makes is on offer; choosing it puts the semicolon back.
    let call = editor.read_with(cx, |e, _| {
        (0..e.buf.line_count())
            .find(|line| e.buf.line_string(*line).contains("x = sum(1, 2);"))
            .unwrap()
    });
    let whole = editor.read_with(cx, |e, _| e.buf.line_string(call));
    editor.update(cx, |editor, cx| {
        editor.feed_keys(keys(&format!("{}G$x", call + 1)), cx);
        editor.feed_keys(keys("j^"), cx);
    });
    wait_for("the error", &editor, cx, |e| {
        e.diagnostic_at_cursor().is_some()
    })
    .await?;
    run(&editor, cx, "editor.code_action", "");
    wait_for("code actions", &editor, cx, |e| {
        !e.code_action_titles().is_empty()
    })
    .await?;
    let titles = editor.read_with(cx, |e, _| e.code_action_titles());
    let Some(fix) = titles.iter().position(|title| title.contains(';')) else {
        return Err(format!("no fix among the code actions: {titles:?}"));
    };
    editor.update(cx, |editor, cx| {
        for _ in 0..fix {
            editor.press(Key::Char('j'), cx);
        }
        editor.press(Key::Enter, cx);
    });
    wait_for("the fix", &editor, cx, |e| {
        e.buf.line_string(call) == whole && e.code_action_titles().is_empty()
    })
    .await
    .map_err(|error| {
        let text = editor.read_with(cx, |e, _| e.buf.text());
        format!(
            "{error}: chose {:?} of {titles:?}, left {text:?}",
            titles[fix]
        )
    })?;
    println!("ok  code action: {:?}", titles[fix]);
    Ok(())
}

fn main() {
    if !has_program("clangd") {
        println!("skipped: clangd not found in PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("med-lsp-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let file = dir.join("main.c");
    std::fs::write(&file, SOURCE).unwrap();

    let cleanup = dir.clone();
    gpui::platform::headless().run(move |cx| {
        med::init_globals(cx);
        // med binds no key: the scenario types the example ones.
        for program in [
            include_str!("../../../examples/vim.mlisp"),
            include_str!("../../../examples/keys.mlisp"),
        ] {
            med::script::eval(program, cx).unwrap();
        }
        let store = cx.new(|_| LspStore::new(dir.clone(), true));
        let editor = cx.new(|cx| Editor::new(Some(file.clone()), SOURCE, Some(store), cx));

        let jumps = Rc::new(RefCell::new(Vec::new()));
        let seen = jumps.clone();
        cx.subscribe(
            &editor,
            move |editor, event: &EditorEvent, cx| match event {
                EditorEvent::OpenLocation { path, position } => {
                    seen.borrow_mut()
                        .push((path.clone(), position.line, position.character));
                }
                // What a workspace does with them, for the one file there is.
                EditorEvent::WorkspaceEdit(edit) => {
                    for (_, edits) in med::lsp::edits_by_file(edit.clone()) {
                        editor.update(cx, |editor, cx| editor.apply_text_edits(&edits, cx));
                    }
                }
                _ => {}
            },
        )
        .detach();

        cx.spawn(async move |cx| {
            let result = scenario(editor, jumps, cx).await;
            let _ = std::fs::remove_dir_all(&cleanup);
            match result {
                Ok(()) => {
                    println!("lsp integration: all checks passed");
                    std::process::exit(0);
                }
                Err(error) => {
                    eprintln!("lsp integration FAILED: {error}");
                    std::process::exit(1);
                }
            }
        })
        .detach();
    });
}
