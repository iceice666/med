//! The transport against a real server (clangd, when installed).

use futures::StreamExt;
use futures::executor::block_on;
use lsp_types as lsp;
use med_lsp::{Client, Incoming};
use serde_json::json;

fn has_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

#[test]
fn initialize_and_receive_diagnostics() {
    if !has_program("clangd") {
        eprintln!("skipped: clangd not found in PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("med-lsp-transport-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let uri: lsp::Uri = format!("file://{}/main.c", dir.display()).parse().unwrap();

    let (client, mut incoming) = Client::spawn("clangd", &[], &dir).unwrap();
    let init = client.request::<lsp::request::Initialize>(lsp::InitializeParams {
        process_id: Some(std::process::id()),
        capabilities: serde_json::from_value(json!({
            "textDocument": { "publishDiagnostics": {} }
        }))
        .unwrap(),
        ..Default::default()
    });
    let result = block_on(init).expect("initialize");
    assert!(result.capabilities.definition_provider.is_some());

    client.notify::<lsp::notification::Initialized>(lsp::InitializedParams {});
    client.notify::<lsp::notification::DidOpenTextDocument>(lsp::DidOpenTextDocumentParams {
        text_document: lsp::TextDocumentItem {
            uri: uri.clone(),
            language_id: "c".into(),
            version: 0,
            text: "int main(void) {\n    return 0\n}\n".into(),
        },
    });

    let diagnostics = block_on(async {
        while let Some(message) = incoming.next().await {
            match message {
                Incoming::Notification { method, params }
                    if method == "textDocument/publishDiagnostics" =>
                {
                    let params: lsp::PublishDiagnosticsParams =
                        serde_json::from_value(params).unwrap();
                    assert_eq!(params.uri, uri);
                    return params.diagnostics;
                }
                Incoming::Request { id, .. } => client.respond(id, Ok(serde_json::Value::Null)),
                Incoming::Exited => panic!("clangd exited early"),
                _ => {}
            }
        }
        unreachable!()
    });
    assert!(
        diagnostics.iter().any(|d| d.message.contains("';'")),
        "{diagnostics:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
