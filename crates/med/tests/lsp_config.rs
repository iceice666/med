//! Language servers as the init file sets them up, against `fake_lsp.py`:
//! a language med knows only by name, two servers for it, what each is
//! told (init options, its configuration, the settings sent to it), their
//! diagnostics together, and a code action run by the server it came from.
//!
//! Its own binary with `harness = false`, as `tests/lsp.rs` is: the servers
//! talk from other threads.

use gpui::{AppContext as _, AsyncApp, Entity};
use med::commands::{Commands, Handler};
use med::editor::Editor;
use med::lsp::LspStore;
use med_vim::Key;
use serde_json::{Value, json};
use std::time::Duration;

fn has_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// What each server says it was told, by its name.
fn told(editor: &Editor) -> Vec<Value> {
    let mut all: Vec<Value> = editor
        .diagnostics()
        .iter()
        .filter_map(|d| serde_json::from_str(&d.message).ok())
        .collect();
    all.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_string());
    all
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
    let seen = editor.read_with(cx, |e, _| told(e));
    Err(format!(
        "timed out waiting for {what}; the servers say {seen:?}"
    ))
}

async fn scenario(editor: Entity<Editor>, cx: &mut AsyncApp) -> Result<(), String> {
    // 1. Each server is told what it was set up with.
    wait_for("both servers to be told everything", &editor, cx, |e| {
        let told = told(e);
        told.len() == 2
            && told.iter().all(|t| !t["asked"].is_null())
            && !told[0]["pushed"].is_null()
    })
    .await?;
    let seen = editor.read_with(cx, |e, _| told(e));
    let a = json!({ "fake": { "sub": { "level": 3 } }, "other": true });
    let expected_a = json!({
        "name": "fake-a",
        "init": { "mode": "fast" },
        "asked": [{ "level": 3 }, a],
        "pushed": a,
        "ran": [],
    });
    if seen[0] != expected_a {
        return Err(format!("fake-a was told {}, not {expected_a}", seen[0]));
    }
    // fake-b has no settings: it is answered null, sent nothing, and has
    // no init options.
    let expected_b = json!({
        "name": "fake-b", "init": null, "asked": [null, null], "pushed": null, "ran": [],
    });
    if seen[1] != expected_b {
        return Err(format!("fake-b was told {}, not {expected_b}", seen[1]));
    }
    println!("ok  settings, configuration and init options");

    // 2. Both servers' code actions are offered, and the one chosen is run
    //    by the server that offered it.
    editor.update(cx, |editor, cx| {
        let Some(Handler::Editor(run)) = cx.global::<Commands>().handler("editor.code_action")
        else {
            panic!("editor.code_action is not a command of the editor's");
        };
        run(editor, &Default::default(), cx);
    });
    wait_for("code actions", &editor, cx, |e| {
        e.code_action_titles().len() == 2
    })
    .await?;
    let titles = editor.read_with(cx, |e, _| e.code_action_titles());
    let Some(b) = titles.iter().position(|t| t == "from fake-b") else {
        return Err(format!("fake-b's action is not among {titles:?}"));
    };
    editor.update(cx, |editor, cx| {
        for _ in 0..b {
            editor.press(Key::Char('j'), cx);
        }
        editor.press(Key::Enter, cx);
    });
    wait_for("fake-b to run its command", &editor, cx, |e| {
        let told = told(e);
        told.len() == 2 && told[1]["ran"] == json!([["fake-b"]]) && told[0]["ran"] == json!([])
    })
    .await?;
    println!("ok  code actions of both, run by the one that offered it");
    Ok(())
}

fn main() {
    if !has_program("python3") {
        println!("skipped: python3 not found in PATH");
        return;
    }
    let server = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_lsp.py");
    let dir = std::env::temp_dir().join(format!("med-lsp-config-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let file = dir.join("main.fake");
    let source = "one\n";
    std::fs::write(&file, source).unwrap();

    let cleanup = dir.clone();
    gpui::platform::headless().run(move |cx| {
        med::init_globals(cx);
        let script = server.display().to_string();
        let program = format!(
            r#"
            (deflanguage "fake" :extensions '("fake"))
            (lsp-server "fake-a" :command "python3" :args '({script:?} "fake-a")
              :languages '("fake")
              :init-options '(:mode "fast")
              :settings '(:fake (:sub (:level 3)) :other true))
            (lsp-server "fake-b" :command "python3" :args '({script:?} "fake-b")
              :languages '("fake"))
            "#
        );
        med::script::eval(&program, cx).unwrap();
        for program in [
            include_str!("../../../examples/vim.mlisp"),
            include_str!("../../../examples/keys.mlisp"),
        ] {
            med::script::eval(program, cx).unwrap();
        }
        let store = cx.new(|_| LspStore::new(dir.clone(), true));
        let editor = cx.new(|cx| Editor::new(Some(file.clone()), source, Some(store), cx));

        cx.spawn(async move |cx| {
            let result = scenario(editor, cx).await;
            let _ = std::fs::remove_dir_all(&cleanup);
            match result {
                Ok(()) => {
                    println!("lsp config integration: all checks passed");
                    std::process::exit(0);
                }
                Err(error) => {
                    eprintln!("lsp config integration FAILED: {error}");
                    std::process::exit(1);
                }
            }
        })
        .detach();
    });
}
