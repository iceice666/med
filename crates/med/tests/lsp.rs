//! Language-server integration, against a real clangd.
//!
//! This is its own binary with `harness = false`: the server talks to us
//! from other threads, so it needs GPUI's real executor on the real main
//! thread rather than the deterministic test one.

use gpui::{AppContext as _, AsyncApp, Entity};
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
        cx.set_global(med::VimGlobal(med_vim::Shared::default()));
        let store = cx.new(|_| LspStore::new(dir.clone(), true));
        let editor = cx.new(|cx| Editor::new(Some(file.clone()), SOURCE, Some(store), cx));

        let jumps = Rc::new(RefCell::new(Vec::new()));
        let seen = jumps.clone();
        cx.subscribe(&editor, move |_, event: &EditorEvent, _| {
            if let EditorEvent::OpenLocation { path, position } = event {
                seen.borrow_mut()
                    .push((path.clone(), position.line, position.character));
            }
        })
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
