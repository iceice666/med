//! Language server management: which server speaks which language, starting
//! them on demand, document sync, and collected diagnostics.

use crate::syntax::LangInfo;
use futures::StreamExt;
use gpui::{Context, EventEmitter, Task};
use lsp_types as lsp;
use med_lsp::{Client, Incoming, RpcError};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Candidate = (&'static str, &'static [&'static str]);

/// Servers to try for a language, in order of preference.
fn candidates(lang: &str) -> &'static [Candidate] {
    match lang {
        "rust" => &[("rust-analyzer", &[])],
        "c" | "cpp" => &[("clangd", &[])],
        "python" => &[
            ("basedpyright-langserver", &["--stdio"]),
            ("pyright-langserver", &["--stdio"]),
            ("pylsp", &[]),
        ],
        "javascript" | "typescript" | "tsx" => &[("typescript-language-server", &["--stdio"])],
        "go" => &[("gopls", &[])],
        "nix" => &[("nixd", &[]), ("nil", &[])],
        "lua" => &[("lua-language-server", &[])],
        "zig" => &[("zls", &[])],
        "swift" => &[("sourcekit-lsp", &[])],
        "toml" => &[("taplo", &["lsp", "stdio"])],
        "json" => &[("vscode-json-language-server", &["--stdio"])],
        "html" => &[("vscode-html-language-server", &["--stdio"])],
        "css" => &[("vscode-css-language-server", &["--stdio"])],
        "yaml" => &[("yaml-language-server", &["--stdio"])],
        "bash" => &[("bash-language-server", &["start"])],
        _ => &[],
    }
}

fn in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

pub fn path_to_uri(path: &Path) -> Option<lsp::Uri> {
    let mut out = String::from("file://");
    for byte in path.to_str()?.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out.parse().ok()
}

pub fn uri_to_path(uri: &lsp::Uri) -> Option<PathBuf> {
    let rest = uri.as_str().strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = rest
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

/// A message waiting for its server to finish initializing. The flag is
/// whether that server takes incremental changes.
type Queued = Box<dyn FnOnce(&Client, bool)>;

struct Server {
    client: Arc<Client>,
    ready: bool,
    incremental: bool,
    /// Messages held back until the server has been initialized.
    queued: Vec<Queued>,
    triggers: Vec<char>,
    /// The characters after which the server has a signature to show.
    signature_triggers: Vec<char>,
    status: Option<String>,
    _reader: Task<()>,
}

pub enum LspEvent {
    /// Diagnostics for this file changed.
    Diagnostics(PathBuf),
    /// A server wants files edited: what a command it was asked to run
    /// comes to.
    ApplyEdit(lsp::WorkspaceEdit),
    /// A server started, stopped, or reported progress.
    Status,
}

pub struct LspStore {
    root: PathBuf,
    /// When false no server is ever started.
    enabled: bool,
    servers: HashMap<&'static str, Server>,
    /// Servers that crashed or failed to start; not retried.
    failed: HashSet<&'static str>,
    docs: HashMap<PathBuf, &'static str>,
    diagnostics: HashMap<PathBuf, Vec<lsp::Diagnostic>>,
}

impl EventEmitter<LspEvent> for LspStore {}

fn client_capabilities() -> lsp::ClientCapabilities {
    serde_json::from_value(json!({
        "general": { "positionEncodings": ["utf-16"] },
        "window": { "workDoneProgress": true },
        "workspace": {
            "configuration": true,
            "workspaceFolders": true,
            "applyEdit": true,
            "workspaceEdit": { "documentChanges": true },
        },
        "textDocument": {
            "rename": {},
            "formatting": {},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": { "valueSet": [
                        "", "quickfix", "refactor", "refactor.extract",
                        "refactor.inline", "refactor.rewrite", "source",
                        "source.organizeImports",
                    ] },
                },
            },
            "signatureHelp": {
                "signatureInformation": {
                    "documentationFormat": ["plaintext", "markdown"],
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true,
                },
            },
            "synchronization": { "didSave": true },
            "publishDiagnostics": { "relatedInformation": true },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": false },
            "references": {},
            "completion": {
                "contextSupport": true,
                "completionItem": {
                    "snippetSupport": false,
                    "documentationFormat": ["plaintext", "markdown"],
                    "insertReplaceSupport": false,
                },
            },
        },
    }))
    .unwrap_or_default()
}

impl LspStore {
    pub fn new(root: PathBuf, enabled: bool) -> Self {
        Self {
            root,
            enabled,
            servers: HashMap::new(),
            failed: HashSet::new(),
            docs: HashMap::new(),
            diagnostics: HashMap::new(),
        }
    }

    fn start(&mut self, program: &'static str, args: &[&str], cx: &mut Context<Self>) -> bool {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let Ok((client, mut incoming)) = Client::spawn(program, &args, &self.root) else {
            self.failed.insert(program);
            return false;
        };
        let client = Arc::new(client);
        let root_uri = path_to_uri(&self.root);
        let folder_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        #[allow(deprecated)]
        let params = lsp::InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: root_uri.clone(),
            capabilities: client_capabilities(),
            workspace_folders: root_uri.map(|uri| {
                vec![lsp::WorkspaceFolder {
                    uri,
                    name: folder_name,
                }]
            }),
            client_info: Some(lsp::ClientInfo {
                name: "med".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };
        let init = client.request::<lsp::request::Initialize>(params);
        let reader = cx.spawn(async move |this, cx| {
            let result = init.await;
            if let Err(error) = &result {
                eprintln!("med: {program} failed to initialize: {error:#}");
            }
            let ok = this
                .update(cx, |this, cx| this.on_initialized(program, result.ok(), cx))
                .unwrap_or(false);
            if !ok {
                return;
            }
            while let Some(message) = incoming.next().await {
                let exited = matches!(message, Incoming::Exited);
                if this
                    .update(cx, |this, cx| this.on_incoming(program, message, cx))
                    .is_err()
                    || exited
                {
                    break;
                }
            }
        });
        self.servers.insert(
            program,
            Server {
                client,
                ready: false,
                incremental: false,
                queued: Vec::new(),
                triggers: Vec::new(),
                signature_triggers: Vec::new(),
                status: Some("starting".into()),
                _reader: reader,
            },
        );
        cx.emit(LspEvent::Status);
        true
    }

    fn on_initialized(
        &mut self,
        program: &'static str,
        result: Option<lsp::InitializeResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(result) = result else {
            self.servers.remove(program);
            self.failed.insert(program);
            cx.emit(LspEvent::Status);
            return false;
        };
        let Some(server) = self.servers.get_mut(program) else {
            return false;
        };
        let caps = &result.capabilities;
        let change = match &caps.text_document_sync {
            Some(lsp::TextDocumentSyncCapability::Kind(kind)) => Some(*kind),
            Some(lsp::TextDocumentSyncCapability::Options(options)) => options.change,
            None => None,
        };
        server.incremental = change == Some(lsp::TextDocumentSyncKind::INCREMENTAL);
        server.triggers = caps
            .completion_provider
            .as_ref()
            .and_then(|c| c.trigger_characters.as_ref())
            .map(|chars| chars.iter().filter_map(|s| s.chars().next()).collect())
            .unwrap_or_default();
        server.signature_triggers = caps
            .signature_help_provider
            .as_ref()
            .and_then(|c| c.trigger_characters.as_ref())
            .map(|chars| chars.iter().filter_map(|s| s.chars().next()).collect())
            .unwrap_or_default();
        server.ready = true;
        server.status = None;
        server
            .client
            .notify::<lsp::notification::Initialized>(lsp::InitializedParams {});
        for message in std::mem::take(&mut server.queued) {
            message(&server.client, server.incremental);
        }
        cx.emit(LspEvent::Status);
        true
    }

    fn on_incoming(&mut self, program: &'static str, message: Incoming, cx: &mut Context<Self>) {
        match message {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    let Ok(params) =
                        serde_json::from_value::<lsp::PublishDiagnosticsParams>(params)
                    else {
                        return;
                    };
                    if let Some(path) = uri_to_path(&params.uri) {
                        self.diagnostics.insert(path.clone(), params.diagnostics);
                        cx.emit(LspEvent::Diagnostics(path));
                    }
                }
                "$/progress" => {
                    let value = &params["value"];
                    let status = match value["kind"].as_str() {
                        Some("end") | None => None,
                        Some(_) => {
                            let mut text = value["title"].as_str().unwrap_or_default().to_string();
                            if let Some(message) = value["message"].as_str() {
                                if !text.is_empty() {
                                    text.push(' ');
                                }
                                text.push_str(message);
                            }
                            if let Some(percent) = value["percentage"].as_u64() {
                                text.push_str(&format!(" {percent}%"));
                            }
                            Some(text)
                        }
                    };
                    if let Some(server) = self.servers.get_mut(program) {
                        // `report` messages often omit the title; keep the
                        // previous text rather than flashing an empty one.
                        if status.as_deref() != Some("") {
                            server.status = status;
                            cx.emit(LspEvent::Status);
                        }
                    }
                }
                _ => {}
            },
            Incoming::Request { id, method, params } => {
                let Some(server) = self.servers.get(program) else {
                    return;
                };
                let result = match method.as_str() {
                    "workspace/configuration" => {
                        let count = params["items"].as_array().map_or(0, Vec::len);
                        Ok(Value::Array(vec![Value::Null; count]))
                    }
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create" => Ok(Value::Null),
                    "workspace/applyEdit" => {
                        let params =
                            serde_json::from_value::<lsp::ApplyWorkspaceEditParams>(params);
                        let applied = params.is_ok();
                        if let Ok(params) = params {
                            cx.emit(LspEvent::ApplyEdit(params.edit));
                        }
                        Ok(json!({ "applied": applied }))
                    }
                    "workspace/workspaceFolders" => Ok(json!([{
                        "uri": path_to_uri(&self.root).map(|u| u.as_str().to_string()),
                        "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()),
                    }])),
                    _ => Err(RpcError {
                        code: -32601,
                        message: format!("unsupported request {method}"),
                    }),
                };
                server.client.respond(id, result);
            }
            Incoming::Exited => {
                self.servers.remove(program);
                self.failed.insert(program);
                cx.emit(LspEvent::Status);
            }
        }
    }

    fn server(&self, path: &Path) -> Option<&Server> {
        self.servers.get(self.docs.get(path)?)
    }

    fn ready_server(&self, path: &Path) -> Option<&Server> {
        self.server(path).filter(|s| s.ready)
    }

    /// Send now, or once the document's server is initialized.
    fn send(&mut self, path: &Path, message: impl FnOnce(&Client, bool) + 'static) {
        let Some(program) = self.docs.get(path) else {
            return;
        };
        let Some(server) = self.servers.get_mut(program) else {
            return;
        };
        if server.ready {
            message(&server.client, server.incremental);
        } else {
            server.queued.push(Box::new(message));
        }
    }

    // ---- document lifecycle --------------------------------------------

    /// Announce an opened file, starting its language server if needed.
    pub fn open(
        &mut self,
        path: &Path,
        lang: &'static LangInfo,
        text: String,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled {
            return;
        }
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        let running = candidates(lang.name)
            .iter()
            .find(|(program, _)| self.servers.contains_key(program))
            .map(|(program, _)| *program);
        let mut program = running;
        if program.is_none() {
            for (candidate, args) in candidates(lang.name) {
                if !self.failed.contains(candidate)
                    && in_path(candidate)
                    && self.start(candidate, args, cx)
                {
                    program = Some(*candidate);
                    break;
                }
            }
        }
        let Some(program) = program else {
            return;
        };
        self.docs.insert(path.to_path_buf(), program);
        let language_id = lang.lsp_id.to_string();
        self.send(path, move |client, _| {
            client.notify::<lsp::notification::DidOpenTextDocument>(
                lsp::DidOpenTextDocumentParams {
                    text_document: lsp::TextDocumentItem {
                        uri,
                        language_id,
                        version: 0,
                        text,
                    },
                },
            );
        });
    }

    /// Report edits. `full_text` is only called for servers that cannot take
    /// incremental changes (or are not initialized yet).
    pub fn change(
        &mut self,
        path: &Path,
        version: i32,
        changes: Vec<lsp::TextDocumentContentChangeEvent>,
        full_text: impl FnOnce() -> String,
    ) {
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        let Some(server) = self.server(path) else {
            return;
        };
        let full = if server.ready && server.incremental {
            None
        } else {
            Some(full_text())
        };
        self.send(path, move |client, incremental| {
            let content_changes = match full {
                Some(text) if !incremental => vec![lsp::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
                _ => changes,
            };
            client.notify::<lsp::notification::DidChangeTextDocument>(
                lsp::DidChangeTextDocumentParams {
                    text_document: lsp::VersionedTextDocumentIdentifier { uri, version },
                    content_changes,
                },
            );
        });
    }

    pub fn save(&mut self, path: &Path) {
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        self.send(path, move |client, _| {
            client.notify::<lsp::notification::DidSaveTextDocument>(
                lsp::DidSaveTextDocumentParams {
                    text_document: lsp::TextDocumentIdentifier { uri },
                    text: None,
                },
            );
        });
    }

    pub fn close(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(uri) = path_to_uri(path) {
            self.send(path, move |client, _| {
                client.notify::<lsp::notification::DidCloseTextDocument>(
                    lsp::DidCloseTextDocumentParams {
                        text_document: lsp::TextDocumentIdentifier { uri },
                    },
                );
            });
        }
        self.docs.remove(path);
        if self.diagnostics.remove(path).is_some() {
            cx.emit(LspEvent::Diagnostics(path.to_path_buf()));
        }
    }

    // ---- queries -------------------------------------------------------

    pub fn diagnostics(&self, path: &Path) -> &[lsp::Diagnostic] {
        self.diagnostics.get(path).map_or(&[], Vec::as_slice)
    }

    /// The diagnostics of every file that has any.
    pub fn all_diagnostics(&self) -> impl Iterator<Item = (&PathBuf, &Vec<lsp::Diagnostic>)> {
        self.diagnostics
            .iter()
            .filter(|(_, diagnostics)| !diagnostics.is_empty())
    }

    /// (errors, warnings) across every file with diagnostics.
    pub fn diagnostic_counts(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        for diagnostic in self.diagnostics.values().flatten() {
            match diagnostic.severity {
                Some(lsp::DiagnosticSeverity::ERROR) => counts.0 += 1,
                Some(lsp::DiagnosticSeverity::WARNING) => counts.1 += 1,
                _ => {}
            }
        }
        counts
    }

    /// Status-bar text for the server attached to `path`.
    pub fn status(&self, path: &Path) -> Option<String> {
        let program = self.docs.get(path)?;
        let server = self.servers.get(program)?;
        Some(match &server.status {
            Some(status) => format!("{program}: {status}"),
            None => program.to_string(),
        })
    }

    /// Characters that should trigger completion for `path`.
    pub fn completion_triggers(&self, path: &Path) -> &[char] {
        self.ready_server(path)
            .map_or(&[], |s| s.triggers.as_slice())
    }

    /// Characters after which to ask for the signature of what is called.
    pub fn signature_triggers(&self, path: &Path) -> &[char] {
        self.ready_server(path)
            .map_or(&[], |s| s.signature_triggers.as_slice())
    }

    fn position_params(
        path: &Path,
        position: lsp::Position,
    ) -> Option<lsp::TextDocumentPositionParams> {
        Some(lsp::TextDocumentPositionParams {
            text_document: lsp::TextDocumentIdentifier {
                uri: path_to_uri(path)?,
            },
            position,
        })
    }

    pub fn hover(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::Hover>>> + use<>> {
        let server = self.ready_server(path)?;
        Some(
            server
                .client
                .request::<lsp::request::HoverRequest>(lsp::HoverParams {
                    text_document_position_params: Self::position_params(path, position)?,
                    work_done_progress_params: Default::default(),
                }),
        )
    }

    pub fn definition(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<>>
    {
        let server = self.ready_server(path)?;
        Some(
            server
                .client
                .request::<lsp::request::GotoDefinition>(lsp::GotoDefinitionParams {
                    text_document_position_params: Self::position_params(path, position)?,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                }),
        )
    }

    pub fn references(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<Vec<lsp::Location>>>> + use<>> {
        let server = self.ready_server(path)?;
        Some(
            server
                .client
                .request::<lsp::request::References>(lsp::ReferenceParams {
                    text_document_position: Self::position_params(path, position)?,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: lsp::ReferenceContext {
                        include_declaration: true,
                    },
                }),
        )
    }

    pub fn completion(
        &self,
        path: &Path,
        position: lsp::Position,
        trigger: Option<char>,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::CompletionResponse>>> + use<>> {
        let server = self.ready_server(path)?;
        let context = match trigger {
            Some(c) => lsp::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::TRIGGER_CHARACTER,
                trigger_character: Some(c.to_string()),
            },
            None => lsp::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
        };
        Some(
            server
                .client
                .request::<lsp::request::Completion>(lsp::CompletionParams {
                    text_document_position: Self::position_params(path, position)?,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(context),
                }),
        )
    }
}

impl LspStore {
    pub fn signature_help(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::SignatureHelp>>> + use<>> {
        let server = self.ready_server(path)?;
        Some(server.client.request::<lsp::request::SignatureHelpRequest>(
            lsp::SignatureHelpParams {
                context: None,
                text_document_position_params: Self::position_params(path, position)?,
                work_done_progress_params: Default::default(),
            },
        ))
    }

    pub fn rename(
        &self,
        path: &Path,
        position: lsp::Position,
        new_name: String,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::WorkspaceEdit>>> + use<>> {
        let server = self.ready_server(path)?;
        Some(
            server
                .client
                .request::<lsp::request::Rename>(lsp::RenameParams {
                    text_document_position: Self::position_params(path, position)?,
                    new_name,
                    work_done_progress_params: Default::default(),
                }),
        )
    }

    /// What can be done about `range`: fixes for the diagnostics in it,
    /// and refactorings.
    pub fn code_actions(
        &self,
        path: &Path,
        range: lsp::Range,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::CodeActionResponse>>> + use<>> {
        let server = self.ready_server(path)?;
        let touches =
            |d: &&lsp::Diagnostic| d.range.start <= range.end && d.range.end >= range.start;
        let diagnostics = self.diagnostics(path).iter().filter(touches).cloned();
        Some(
            server
                .client
                .request::<lsp::request::CodeActionRequest>(lsp::CodeActionParams {
                    text_document: lsp::TextDocumentIdentifier {
                        uri: path_to_uri(path)?,
                    },
                    range,
                    context: lsp::CodeActionContext {
                        diagnostics: diagnostics.collect(),
                        only: None,
                        trigger_kind: Some(lsp::CodeActionTriggerKind::INVOKED),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                }),
        )
    }

    /// Have the server of `path` run one of its commands. What that edits
    /// comes back as [`LspEvent::ApplyEdit`].
    pub fn execute_command(&self, path: &Path, command: lsp::Command) {
        let Some(server) = self.ready_server(path) else {
            return;
        };
        let request =
            server
                .client
                .request::<lsp::request::ExecuteCommand>(lsp::ExecuteCommandParams {
                    command: command.command,
                    arguments: command.arguments.unwrap_or_default(),
                    work_done_progress_params: Default::default(),
                });
        // It is on its way already, and nothing waits for the answer.
        drop(request);
    }

    pub fn formatting(
        &self,
        path: &Path,
        tab_size: u32,
        insert_spaces: bool,
    ) -> Option<impl Future<Output = anyhow::Result<Option<Vec<lsp::TextEdit>>>> + use<>> {
        let server = self.ready_server(path)?;
        Some(
            server
                .client
                .request::<lsp::request::Formatting>(lsp::DocumentFormattingParams {
                    text_document: lsp::TextDocumentIdentifier {
                        uri: path_to_uri(path)?,
                    },
                    options: lsp::FormattingOptions {
                        tab_size,
                        insert_spaces,
                        ..Default::default()
                    },
                    work_done_progress_params: Default::default(),
                }),
        )
    }
}

/// The edits a server asked for, file by file. Creating, renaming and
/// deleting files is left out: only text is edited.
pub fn edits_by_file(edit: lsp::WorkspaceEdit) -> Vec<(PathBuf, Vec<lsp::TextEdit>)> {
    let mut files: Vec<(PathBuf, Vec<lsp::TextEdit>)> = Vec::new();
    let mut add = |uri: &lsp::Uri, edits: Vec<lsp::TextEdit>| {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        match files.iter_mut().find(|(known, _)| *known == path) {
            Some((_, known)) => known.extend(edits),
            None => files.push((path, edits)),
        }
    };
    let plain = |edit: lsp::OneOf<lsp::TextEdit, lsp::AnnotatedTextEdit>| match edit {
        lsp::OneOf::Left(edit) => edit,
        lsp::OneOf::Right(annotated) => annotated.text_edit,
    };
    match edit.document_changes {
        Some(lsp::DocumentChanges::Edits(documents)) => {
            for document in documents {
                let edits = document.edits.into_iter().map(plain).collect();
                add(&document.text_document.uri, edits);
            }
        }
        Some(lsp::DocumentChanges::Operations(operations)) => {
            for operation in operations {
                if let lsp::DocumentChangeOperation::Edit(document) = operation {
                    let edits = document.edits.into_iter().map(plain).collect();
                    add(&document.text_document.uri, edits);
                }
            }
        }
        None => {
            for (uri, edits) in edit.changes.into_iter().flatten() {
                add(&uri, edits);
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let path = Path::new("/tmp/my dir/檔案 #1.rs");
        let uri = path_to_uri(path).unwrap();
        assert_eq!(
            uri.as_str(),
            "file:///tmp/my%20dir/%E6%AA%94%E6%A1%88%20%231.rs"
        );
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }
}
