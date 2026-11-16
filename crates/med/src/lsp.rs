//! Language server management: which servers speak which language
//! (`lsp_config`), starting them on demand, document sync, and collected
//! diagnostics.
//!
//! A file can have several servers (tsserver and tailwind). Every one of
//! them is told of its text and gives diagnostics, code actions,
//! completions and hover; anything else asked about the file goes to the
//! first that says it can answer.

use crate::env::{EnvCell, ProjectEnv};
use crate::lsp_config::{self, ServerConfig};
use crate::syntax::LangInfo;
use futures::StreamExt;
use gpui::{Context, EventEmitter, Task};
use lsp_types as lsp;
use med_lsp::{Client, Incoming, RpcError};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A server, by the name it is configured under.
pub type ServerName = Arc<str>;

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
    /// What it was started with, and is to be told.
    config: ServerConfig,
    /// What it said it can do, once it is ready.
    caps: lsp::ServerCapabilities,
    ready: bool,
    incremental: bool,
    /// Messages held back until the server has been initialized.
    queued: Vec<Queued>,
    triggers: Vec<char>,
    /// The characters after which the server has a signature to show.
    signature_triggers: Vec<char>,
    /// Whether the server says what it knows inline, as inlay hints.
    inlay_hints: bool,
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
    /// A server has other inlay hints to give, for any of its files.
    InlayHintsRefresh,
}

pub struct LspStore {
    root: PathBuf,
    /// When false no server is ever started.
    enabled: bool,
    servers: HashMap<ServerName, Server>,
    /// Servers that crashed or failed to start; not retried.
    failed: HashSet<ServerName>,
    /// The environment servers are started in.
    env: Arc<EnvCell>,
    /// Files opened while the environment was still being worked out,
    /// with their language and latest text.
    waiting: HashMap<PathBuf, (&'static LangInfo, String)>,
    /// The servers of each open file, the one asked first first.
    docs: HashMap<PathBuf, Vec<ServerName>>,
    /// What each server said of each file.
    published: HashMap<PathBuf, Vec<(ServerName, Vec<lsp::Diagnostic>)>>,
    /// All of those, file by file.
    diagnostics: HashMap<PathBuf, Vec<lsp::Diagnostic>>,
}

/// Whether a server offers something it may also have turned off.
fn offers<T>(capability: &Option<lsp::OneOf<bool, T>>) -> bool {
    !matches!(capability, None | Some(lsp::OneOf::Left(false)))
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
            "inlayHint": { "refreshSupport": true },
        },
        "textDocument": {
            "rename": {},
            "formatting": {},
            "inlayHint": {},
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
            "declaration": { "linkSupport": false },
            "typeDefinition": { "linkSupport": false },
            "implementation": { "linkSupport": false },
            "references": {},
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
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
    /// Servers start in med's own environment.
    pub fn new(root: PathBuf, enabled: bool) -> Self {
        Self::with_env(root, enabled, EnvCell::ready(ProjectEnv::inherited()))
    }

    /// Servers start in `env`, and files opened before it is there wait.
    pub fn with_env(root: PathBuf, enabled: bool, env: Arc<EnvCell>) -> Self {
        Self {
            root,
            enabled,
            servers: HashMap::new(),
            failed: HashSet::new(),
            env,
            waiting: HashMap::new(),
            docs: HashMap::new(),
            published: HashMap::new(),
            diagnostics: HashMap::new(),
        }
    }

    /// The environment is there: open the files that waited for it.
    pub fn env_ready(&mut self, cx: &mut Context<Self>) {
        for (path, (lang, text)) in std::mem::take(&mut self.waiting) {
            self.open(&path, lang, text, cx);
        }
        cx.emit(LspEvent::Status);
    }

    /// Stop every server and forget which failed, as when the environment
    /// changes; the files are to be opened again.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.servers.clear();
        self.failed.clear();
        self.docs.clear();
        self.waiting.clear();
        self.published.clear();
        for (path, _) in std::mem::take(&mut self.diagnostics) {
            cx.emit(LspEvent::Diagnostics(path));
        }
        cx.emit(LspEvent::Status);
    }

    fn start(&mut self, env: &ProjectEnv, config: ServerConfig, cx: &mut Context<Self>) -> bool {
        let program: ServerName = config.name.clone();
        let mut command = env.command(&config.command);
        command.args(&config.args).current_dir(&self.root);
        let Ok((client, mut incoming)) = Client::spawn_command(&program, command) else {
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
            initialization_options: config.init_options.clone(),
            ..Default::default()
        };
        let init = client.request::<lsp::request::Initialize>(params);
        let name = program.clone();
        let reader = cx.spawn(async move |this, cx| {
            let program = name;
            let result = init.await;
            if let Err(error) = &result {
                eprintln!("med: {program} failed to initialize: {error:#}");
            }
            let ok = this
                .update(cx, |this, cx| {
                    this.on_initialized(&program, result.ok(), cx)
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            while let Some(message) = incoming.next().await {
                let exited = matches!(message, Incoming::Exited);
                if this
                    .update(cx, |this, cx| this.on_incoming(&program, message, cx))
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
                config,
                caps: lsp::ServerCapabilities::default(),
                ready: false,
                incremental: false,
                queued: Vec::new(),
                triggers: Vec::new(),
                signature_triggers: Vec::new(),
                inlay_hints: false,
                status: Some("starting".into()),
                _reader: reader,
            },
        );
        cx.emit(LspEvent::Status);
        true
    }

    fn on_initialized(
        &mut self,
        program: &str,
        result: Option<lsp::InitializeResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(result) = result else {
            self.servers.remove(program);
            self.failed.insert(program.into());
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
        server.inlay_hints = offers(&caps.inlay_hint_provider);
        server.caps = result.capabilities;
        server.ready = true;
        server.status = None;
        server
            .client
            .notify::<lsp::notification::Initialized>(lsp::InitializedParams {});
        // As well as answering for them, as lspconfig does: some servers
        // only read what they are sent.
        if server
            .config
            .settings
            .as_object()
            .is_some_and(|s| !s.is_empty())
        {
            server
                .client
                .notify::<lsp::notification::DidChangeConfiguration>(
                    lsp::DidChangeConfigurationParams {
                        settings: server.config.settings.clone(),
                    },
                );
        }
        for message in std::mem::take(&mut server.queued) {
            message(&server.client, server.incremental);
        }
        cx.emit(LspEvent::Status);
        true
    }

    fn on_incoming(&mut self, program: &str, message: Incoming, cx: &mut Context<Self>) {
        match message {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    let Ok(params) =
                        serde_json::from_value::<lsp::PublishDiagnosticsParams>(params)
                    else {
                        return;
                    };
                    if let Some(path) = uri_to_path(&params.uri) {
                        self.publish(program, path.clone(), params.diagnostics);
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
                        Ok(configuration(&server.config.settings, &params))
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
                    "workspace/inlayHint/refresh" => {
                        cx.emit(LspEvent::InlayHintsRefresh);
                        Ok(Value::Null)
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
                self.failed.insert(program.into());
                cx.emit(LspEvent::Status);
            }
        }
    }

    /// The servers of `path` that are ready, the one asked first first.
    fn ready_servers<'a>(&'a self, path: &Path) -> impl Iterator<Item = &'a Server> + 'a {
        self.docs
            .get(path)
            .into_iter()
            .flatten()
            .filter_map(|name| self.servers.get(name))
            .filter(|server| server.ready)
    }

    /// Whether every server of `path` that is running is ready.
    fn all_ready(&self, path: &Path) -> bool {
        self.docs
            .get(path)
            .into_iter()
            .flatten()
            .filter_map(|name| self.servers.get(name))
            .all(|server| server.ready)
    }

    /// The first ready server of `path` that says it can do what `can`
    /// asks. Failing that, once all are ready, the first, which may know
    /// more than it said; while one is starting it may be the one to ask.
    fn ready_server(
        &self,
        path: &Path,
        can: impl Fn(&lsp::ServerCapabilities) -> bool,
    ) -> Option<&Server> {
        self.ready_servers(path)
            .find(|server| can(&server.caps))
            .or_else(|| {
                self.all_ready(path)
                    .then(|| self.ready_servers(path).next())
                    .flatten()
            })
    }

    /// Every ready server of `path` that says it can do what `can` asks,
    /// the one asked first first. Failing that, once all are ready, the
    /// first, as with [`Self::ready_server`].
    fn offering_servers(
        &self,
        path: &Path,
        can: impl Fn(&lsp::ServerCapabilities) -> bool,
    ) -> Vec<&Server> {
        let mut servers: Vec<&Server> = self
            .ready_servers(path)
            .filter(|server| can(&server.caps))
            .collect();
        if servers.is_empty() && self.all_ready(path) {
            servers.extend(self.ready_servers(path).next());
        }
        servers
    }

    /// Send to one server now, or once it is initialized.
    fn send_to(&mut self, name: &str, message: Queued) {
        let Some(server) = self.servers.get_mut(name) else {
            return;
        };
        if server.ready {
            message(&server.client, server.incremental);
        } else {
            server.queued.push(message);
        }
    }

    /// Send to every server of the document, each its own message.
    fn send(&mut self, path: &Path, message: impl Fn() -> Queued) {
        for name in self.docs.get(path).cloned().unwrap_or_default() {
            self.send_to(&name, message());
        }
    }

    /// What a server said of a file, in place of what it said before.
    fn publish(&mut self, server: &str, path: PathBuf, diagnostics: Vec<lsp::Diagnostic>) {
        let published = self.published.entry(path.clone()).or_default();
        match published.iter_mut().find(|(name, _)| &**name == server) {
            Some((_, known)) => *known = diagnostics,
            None => published.push((server.into(), diagnostics)),
        }
        let all = published
            .iter()
            .flat_map(|(_, d)| d.iter().cloned())
            .collect();
        self.diagnostics.insert(path, all);
    }

    // ---- document lifecycle --------------------------------------------

    /// Announce an opened file, starting its language servers if needed.
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
        let Some(env) = self.env.get() else {
            self.waiting.insert(path.to_path_buf(), (lang, text));
            cx.emit(LspEvent::Status);
            return;
        };
        // Of each group, one: the one running, or else the first that starts.
        let configs = lsp_config::servers_for(lang.name, cx);
        let mut groups: Vec<Vec<ServerConfig>> = Vec::new();
        for config in configs {
            match groups.iter_mut().find(|g| g[0].group == config.group) {
                Some(group) => group.push(config),
                None => groups.push(vec![config]),
            }
        }
        let mut names = Vec::new();
        for group in groups {
            if let Some(running) = group.iter().find(|c| self.servers.contains_key(&c.name)) {
                names.push(running.name.clone());
                continue;
            }
            for config in group {
                let name = config.name.clone();
                if !self.failed.contains(&name)
                    && env.find_program(&config.command).is_some()
                    && self.start(&env, config, cx)
                {
                    names.push(name);
                    break;
                }
            }
        }
        if names.is_empty() {
            return;
        }
        self.docs.insert(path.to_path_buf(), names);
        let language_id = lang.lsp_id.to_string();
        self.send(path, || {
            let (uri, language_id, text) = (uri.clone(), language_id.clone(), text.clone());
            Box::new(move |client, _| {
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
            })
        });
    }

    /// Report edits. `full_text` is only called if some server cannot take
    /// incremental changes (or is not initialized yet).
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
        if let Some((_, text)) = self.waiting.get_mut(path) {
            *text = full_text();
            return;
        }
        let Some(names) = self.docs.get(path) else {
            return;
        };
        let wants_full = names
            .iter()
            .filter_map(|name| self.servers.get(name))
            .any(|server| !(server.ready && server.incremental));
        let full = wants_full.then(full_text);
        self.send(path, || {
            let (uri, changes, full) = (uri.clone(), changes.clone(), full.clone());
            Box::new(move |client, incremental| {
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
            })
        });
    }

    pub fn save(&mut self, path: &Path) {
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        self.send(path, || {
            let uri = uri.clone();
            Box::new(move |client, _| {
                client.notify::<lsp::notification::DidSaveTextDocument>(
                    lsp::DidSaveTextDocumentParams {
                        text_document: lsp::TextDocumentIdentifier { uri },
                        text: None,
                    },
                );
            })
        });
    }

    pub fn close(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(uri) = path_to_uri(path) {
            self.send(path, || {
                let uri = uri.clone();
                Box::new(move |client, _| {
                    client.notify::<lsp::notification::DidCloseTextDocument>(
                        lsp::DidCloseTextDocumentParams {
                            text_document: lsp::TextDocumentIdentifier { uri },
                        },
                    );
                })
            });
        }
        self.docs.remove(path);
        self.waiting.remove(path);
        self.published.remove(path);
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

    /// Status-bar text for the servers attached to `path`.
    pub fn status(&self, path: &Path) -> Option<String> {
        if self.waiting.contains_key(path) {
            return Some("loading the environment".into());
        }
        let parts: Vec<String> = self
            .docs
            .get(path)?
            .iter()
            .filter_map(|name| {
                let server = self.servers.get(name)?;
                Some(match &server.status {
                    Some(status) => format!("{name}: {status}"),
                    None => name.to_string(),
                })
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Characters after which to ask for the signature of what is called.
    pub fn signature_triggers(&self, path: &Path) -> &[char] {
        self.ready_server(path, |c| c.signature_help_provider.is_some())
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

    /// What is said of the symbol at `position`, by every server of
    /// `path` that says anything, the one asked first first.
    pub fn hover(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = Vec<lsp::Hover>> + use<>> {
        let servers = self.offering_servers(path, |c| {
            c.hover_provider
                .as_ref()
                .is_some_and(|h| !matches!(h, lsp::HoverProviderCapability::Simple(false)))
        });
        if servers.is_empty() {
            return None;
        }
        let params = lsp::HoverParams {
            text_document_position_params: Self::position_params(path, position)?,
            work_done_progress_params: Default::default(),
        };
        let requests: Vec<_> = servers
            .into_iter()
            .map(|server| {
                let request = server
                    .client
                    .request::<lsp::request::HoverRequest>(params.clone());
                // One that fails hides nothing the others have.
                async move { request.await.ok().flatten() }
            })
            .collect();
        Some(async move {
            futures::future::join_all(requests)
                .await
                .into_iter()
                .flatten()
                .collect()
        })
    }

    /// Ask the first server of `path` that says it can, by `can`, where
    /// something of the symbol at `position` is: its definition, its type…
    fn locate<R>(
        &self,
        path: &Path,
        position: lsp::Position,
        can: fn(&lsp::ServerCapabilities) -> bool,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<R>>
    where
        R: lsp::request::Request<
                Params = lsp::GotoDefinitionParams,
                Result = Option<lsp::GotoDefinitionResponse>,
            >,
    {
        let server = self.ready_server(path, can)?;
        Some(server.client.request::<R>(lsp::GotoDefinitionParams {
            text_document_position_params: Self::position_params(path, position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }))
    }

    pub fn definition(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<>>
    {
        self.locate::<lsp::request::GotoDefinition>(path, position, |c| {
            offers(&c.definition_provider)
        })
    }

    pub fn declaration(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<>>
    {
        self.locate::<lsp::request::GotoDeclaration>(path, position, |c| {
            !matches!(
                c.declaration_provider,
                None | Some(lsp::DeclarationCapability::Simple(false))
            )
        })
    }

    /// Where the type of the symbol at `position` is defined.
    pub fn type_definition(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<>>
    {
        self.locate::<lsp::request::GotoTypeDefinition>(path, position, |c| {
            !matches!(
                c.type_definition_provider,
                None | Some(lsp::TypeDefinitionProviderCapability::Simple(false))
            )
        })
    }

    /// Where what is at `position` is implemented: the bodies of a trait's
    /// or an interface's methods, the types that implement it.
    pub fn implementation(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + use<>>
    {
        self.locate::<lsp::request::GotoImplementation>(path, position, |c| {
            !matches!(
                c.implementation_provider,
                None | Some(lsp::ImplementationProviderCapability::Simple(false))
            )
        })
    }

    /// The symbols of `path`, as the first server of it that has them
    /// gives them: a tree, or a list.
    pub fn document_symbols(
        &self,
        path: &Path,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::DocumentSymbolResponse>>> + use<>>
    {
        let server = self.ready_server(path, |c| offers(&c.document_symbol_provider))?;
        Some(
            server
                .client
                .request::<lsp::request::DocumentSymbolRequest>(lsp::DocumentSymbolParams {
                    text_document: lsp::TextDocumentIdentifier {
                        uri: path_to_uri(path)?,
                    },
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
        let server = self.ready_server(path, |c| offers(&c.references_provider))?;
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

    /// The servers of `path` to ask for completions after `trigger`, or
    /// when asked or a word is typed (`None`).
    fn completion_servers(&self, path: &Path, trigger: Option<char>) -> Vec<&Server> {
        let servers: Vec<&Server> = self.ready_servers(path).collect();
        let offers: Vec<(bool, &[char])> = servers
            .iter()
            .map(|s| (s.caps.completion_provider.is_some(), s.triggers.as_slice()))
            .collect();
        completion_askees(&offers, trigger, self.all_ready(path))
            .into_iter()
            .map(|i| servers[i])
            .collect()
    }

    /// Whether typing `c` in `path` asks for completions: whether a server
    /// of it said so.
    pub fn triggers_completion(&self, path: &Path, c: char) -> bool {
        !self.completion_servers(path, Some(c)).is_empty()
    }

    /// What may be typed at `position`, from every server of `path` that
    /// has any, each item with the place of its server among those asked:
    /// the one asked first first.
    pub fn completion(
        &self,
        path: &Path,
        position: lsp::Position,
        trigger: Option<char>,
    ) -> Option<impl Future<Output = Vec<(usize, lsp::CompletionItem)>> + use<>> {
        let servers = self.completion_servers(path, trigger);
        if servers.is_empty() {
            return None;
        }
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
        let params = lsp::CompletionParams {
            text_document_position: Self::position_params(path, position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: Some(context),
        };
        let requests: Vec<_> = servers
            .into_iter()
            .map(|server| {
                let request = server
                    .client
                    .request::<lsp::request::Completion>(params.clone());
                // One that fails hides nothing the others have.
                async move { request.await.ok().flatten() }
            })
            .collect();
        Some(async move { completion_items(futures::future::join_all(requests).await) })
    }
}

/// Of a file's servers, each as whether it offers completions and the
/// characters it said trigger them, the ones to ask after `trigger`, or
/// when asked or a word is typed (`None`): every one that offers them and
/// registered the trigger. With none offering them, once all are ready,
/// the first, which may know more than it said.
fn completion_askees(
    servers: &[(bool, &[char])],
    trigger: Option<char>,
    all_ready: bool,
) -> Vec<usize> {
    let mut askees: Vec<usize> = (0..servers.len()).filter(|&i| servers[i].0).collect();
    if askees.is_empty() && all_ready && !servers.is_empty() {
        askees.push(0);
    }
    if let Some(c) = trigger {
        askees.retain(|&i| servers[i].1.contains(&c));
    }
    askees
}

/// The items of each server's answer, in the order the servers were
/// asked, each with the place of its server.
fn completion_items(
    answers: Vec<Option<lsp::CompletionResponse>>,
) -> Vec<(usize, lsp::CompletionItem)> {
    answers
        .into_iter()
        .enumerate()
        .flat_map(|(rank, answer)| {
            let items = match answer {
                Some(lsp::CompletionResponse::Array(items)) => items,
                Some(lsp::CompletionResponse::List(list)) => list.items,
                None => Vec::new(),
            };
            items.into_iter().map(move |item| (rank, item))
        })
        .collect()
}

impl LspStore {
    pub fn signature_help(
        &self,
        path: &Path,
        position: lsp::Position,
    ) -> Option<impl Future<Output = anyhow::Result<Option<lsp::SignatureHelp>>> + use<>> {
        let server = self.ready_server(path, |c| c.signature_help_provider.is_some())?;
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
        let server = self.ready_server(path, |c| offers(&c.rename_provider))?;
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
    /// and refactorings, from every server of the file that has any, each
    /// with the server it came from.
    pub fn code_actions(
        &self,
        path: &Path,
        range: lsp::Range,
    ) -> Option<impl Future<Output = Vec<(ServerName, lsp::CodeActionOrCommand)>> + use<>> {
        let uri = path_to_uri(path)?;
        let servers = self.offering_servers(path, |c| {
            c.code_action_provider
                .as_ref()
                .is_some_and(|c| !matches!(c, lsp::CodeActionProviderCapability::Simple(false)))
        });
        if servers.is_empty() {
            return None;
        }
        let touches =
            |d: &&lsp::Diagnostic| d.range.start <= range.end && d.range.end >= range.start;
        let requests: Vec<_> = servers
            .into_iter()
            .map(|server| {
                // A server is told of its own diagnostics only.
                let diagnostics = self
                    .published
                    .get(path)
                    .and_then(|p| p.iter().find(|(name, _)| *name == server.config.name))
                    .map(|(_, d)| d.iter().filter(touches).cloned().collect())
                    .unwrap_or_default();
                let name = server.config.name.clone();
                let request = server.client.request::<lsp::request::CodeActionRequest>(
                    lsp::CodeActionParams {
                        text_document: lsp::TextDocumentIdentifier { uri: uri.clone() },
                        range,
                        context: lsp::CodeActionContext {
                            diagnostics,
                            only: None,
                            trigger_kind: Some(lsp::CodeActionTriggerKind::INVOKED),
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                );
                async move {
                    let actions = request.await.ok().flatten().unwrap_or_default();
                    actions.into_iter().map(move |a| (name.clone(), a))
                }
            })
            .collect();
        Some(async move {
            futures::future::join_all(requests)
                .await
                .into_iter()
                .flatten()
                .collect()
        })
    }

    /// Have a server of `path` run one of its commands: `server`, the one
    /// that offered it, or else the first. What that edits comes back as
    /// [`LspEvent::ApplyEdit`].
    pub fn execute_command(&self, path: &Path, server: Option<&str>, command: lsp::Command) {
        let server = match server {
            Some(name) => self.ready_servers(path).find(|s| &*s.config.name == name),
            None => self.ready_server(path, |c| c.execute_command_provider.is_some()),
        };
        let Some(server) = server else {
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

    /// What the server says inline of the text in `range`.
    pub fn inlay_hints(
        &self,
        path: &Path,
        range: lsp::Range,
    ) -> Option<impl Future<Output = anyhow::Result<Option<Vec<lsp::InlayHint>>>> + use<>> {
        let server = self.ready_servers(path).find(|server| server.inlay_hints)?;
        Some(
            server
                .client
                .request::<lsp::request::InlayHintRequest>(lsp::InlayHintParams {
                    text_document: lsp::TextDocumentIdentifier {
                        uri: path_to_uri(path)?,
                    },
                    range,
                    work_done_progress_params: Default::default(),
                }),
        )
    }

    pub fn formatting(
        &self,
        path: &Path,
        tab_size: u32,
        insert_spaces: bool,
    ) -> Option<impl Future<Output = anyhow::Result<Option<Vec<lsp::TextEdit>>>> + use<>> {
        let server = self.ready_server(path, |c| offers(&c.document_formatting_provider))?;
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

/// The answer to `workspace/configuration`: for each item, the part of the
/// settings at its section, or all of them for an item with none.
fn configuration(settings: &Value, params: &Value) -> Value {
    let items = params["items"].as_array().map_or(&[][..], Vec::as_slice);
    Value::Array(
        items
            .iter()
            .map(|item| lsp_config::section(settings, item["section"].as_str()))
            .collect(),
    )
}

/// A place to choose among others, and what to call it in the list:
/// `None` for the line it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub location: lsp::Location,
    pub label: Option<String>,
}

/// Every place an answer to "where is it" names, in its order, each once.
pub fn locations(response: lsp::GotoDefinitionResponse) -> Vec<lsp::Location> {
    let all = match response {
        lsp::GotoDefinitionResponse::Scalar(location) => vec![location],
        lsp::GotoDefinitionResponse::Array(locations) => locations,
        lsp::GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|link| lsp::Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
    };
    let mut seen = Vec::with_capacity(all.len());
    for location in all {
        if !seen.contains(&location) {
            seen.push(location);
        }
    }
    seen
}

/// What a kind of symbol is called.
fn symbol_kind_name(kind: lsp::SymbolKind) -> &'static str {
    match kind {
        lsp::SymbolKind::FILE => "file",
        lsp::SymbolKind::MODULE => "module",
        lsp::SymbolKind::NAMESPACE => "namespace",
        lsp::SymbolKind::PACKAGE => "package",
        lsp::SymbolKind::CLASS => "class",
        lsp::SymbolKind::METHOD => "method",
        lsp::SymbolKind::PROPERTY => "property",
        lsp::SymbolKind::FIELD => "field",
        lsp::SymbolKind::CONSTRUCTOR => "constructor",
        lsp::SymbolKind::ENUM => "enum",
        lsp::SymbolKind::INTERFACE => "interface",
        lsp::SymbolKind::FUNCTION => "function",
        lsp::SymbolKind::VARIABLE => "variable",
        lsp::SymbolKind::CONSTANT => "constant",
        lsp::SymbolKind::STRING => "string",
        lsp::SymbolKind::NUMBER => "number",
        lsp::SymbolKind::BOOLEAN => "boolean",
        lsp::SymbolKind::ARRAY => "array",
        lsp::SymbolKind::OBJECT => "object",
        lsp::SymbolKind::KEY => "key",
        lsp::SymbolKind::NULL => "null",
        lsp::SymbolKind::ENUM_MEMBER => "variant",
        lsp::SymbolKind::STRUCT => "struct",
        lsp::SymbolKind::EVENT => "event",
        lsp::SymbolKind::OPERATOR => "operator",
        lsp::SymbolKind::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

/// The symbols of the file at `uri`, from the top of it down, each called
/// by its kind and name; one inside another is indented under it.
pub fn symbol_places(response: lsp::DocumentSymbolResponse, uri: &lsp::Uri) -> Vec<Place> {
    fn walk(
        symbols: &mut [lsp::DocumentSymbol],
        depth: usize,
        uri: &lsp::Uri,
        out: &mut Vec<Place>,
    ) {
        symbols.sort_by_key(|s| (s.range.start.line, s.range.start.character));
        for symbol in symbols {
            out.push(Place {
                location: lsp::Location {
                    uri: uri.clone(),
                    range: symbol.selection_range,
                },
                label: Some(format!(
                    "{}{} {}",
                    "  ".repeat(depth),
                    symbol_kind_name(symbol.kind),
                    symbol.name
                )),
            });
            if let Some(children) = &mut symbol.children {
                walk(children, depth + 1, uri, out);
            }
        }
    }
    let mut out = Vec::new();
    match response {
        lsp::DocumentSymbolResponse::Nested(mut symbols) => walk(&mut symbols, 0, uri, &mut out),
        lsp::DocumentSymbolResponse::Flat(mut symbols) => {
            symbols.sort_by_key(|s| {
                let start = s.location.range.start;
                (start.line, start.character)
            });
            out.extend(symbols.into_iter().map(|symbol| {
                let name = match &symbol.container_name {
                    Some(container) if !container.is_empty() => {
                        format!("{container}.{}", symbol.name)
                    }
                    _ => symbol.name,
                };
                Place {
                    label: Some(format!("{} {name}", symbol_kind_name(symbol.kind))),
                    location: symbol.location,
                }
            }));
        }
    }
    out
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

    /// A file opened before the environment is there waits for it, keeps
    /// its latest text, and gets the server found on that environment's
    /// PATH, not med's.
    #[cfg(unix)]
    #[gpui::test]
    fn servers_wait_for_the_environment(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("med-lsp-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = dir.join("rust-analyzer");
        std::fs::write(&server, "#!/bin/sh\nexec cat >/dev/null\n").unwrap();
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.join("main.rs");
        let lang = crate::syntax::lang_for_path(&file).unwrap();

        let cell = Arc::new(EnvCell::default());
        let store = cx.new(|_| LspStore::with_env(dir.clone(), true, cell.clone()));
        store.update(cx, |store, cx| {
            store.open(&file, lang, "fn main() {}".into(), cx)
        });
        store.update(cx, |store, _| {
            store.change(&file, 1, Vec::new(), || "fn main() { 1; }".into());
        });
        store.read_with(cx, |store, _| {
            assert_eq!(
                store.status(&file).as_deref(),
                Some("loading the environment")
            );
            assert_eq!(store.waiting[&file].1, "fn main() { 1; }");
        });

        let path = format!("{}:/usr/bin:/bin", dir.display());
        cell.set(ProjectEnv::from_vars([("PATH", path.as_str())]));
        store.update(cx, |store, cx| store.env_ready(cx));
        store.read_with(cx, |store, _| {
            assert!(store.waiting.is_empty());
            assert_eq!(
                store.status(&file).as_deref(),
                Some("rust-analyzer: starting")
            );
        });

        store.update(cx, |store, cx| store.restart(cx));
        store.read_with(cx, |store, _| assert_eq!(store.status(&file), None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every server that offers completions is asked, but after a trigger
    /// only those that registered it; with none offering them, the first,
    /// once all are ready.
    #[test]
    fn completions_are_asked_of_every_server() {
        let ts: &[char] = &['.', '"'];
        let tailwind: &[char] = &['"', ' ', '-'];
        let servers = [(true, ts), (false, &[][..]), (true, tailwind)];
        assert_eq!(completion_askees(&servers, None, true), [0, 2]);
        assert_eq!(completion_askees(&servers, Some('.'), true), [0]);
        assert_eq!(completion_askees(&servers, Some('-'), true), [2]);
        assert_eq!(completion_askees(&servers, Some('"'), false), [0, 2]);
        assert!(completion_askees(&servers, Some('('), true).is_empty());

        let silent = [(false, &[][..]), (false, &[][..])];
        assert_eq!(completion_askees(&silent, None, true), [0]);
        assert!(completion_askees(&silent, None, false).is_empty());
        assert!(completion_askees(&silent, Some('.'), true).is_empty());
        assert!(completion_askees(&[], None, true).is_empty());
    }

    /// Each server's items, in the order they were asked, with its place;
    /// one that gave nothing or failed leaves the others'.
    #[test]
    fn completions_of_every_server_are_merged() {
        let item = |label: &str| lsp::CompletionItem {
            label: label.into(),
            ..Default::default()
        };
        let answers = vec![
            Some(lsp::CompletionResponse::Array(vec![item("useState")])),
            None,
            Some(lsp::CompletionResponse::List(lsp::CompletionList {
                is_incomplete: true,
                items: vec![item("bg-red-500"), item("flex")],
            })),
        ];
        let merged: Vec<_> = completion_items(answers)
            .into_iter()
            .map(|(rank, item)| (rank, item.label))
            .collect();
        assert_eq!(
            merged,
            [
                (0, "useState".to_string()),
                (2, "bg-red-500".to_string()),
                (2, "flex".to_string()),
            ]
        );
    }

    fn range(line: u32, character: u32) -> lsp::Range {
        let at = lsp::Position { line, character };
        lsp::Range { start: at, end: at }
    }

    /// Every place, from any of the three shapes of answer, in its order
    /// and each once; a link goes to the name it targets.
    #[test]
    fn locations_of_every_shape() {
        let uri: lsp::Uri = "file:///p/a.rs".parse().unwrap();
        let at = |line| lsp::Location {
            uri: uri.clone(),
            range: range(line, 0),
        };
        assert_eq!(
            locations(lsp::GotoDefinitionResponse::Scalar(at(1))),
            [at(1)]
        );
        assert_eq!(
            locations(lsp::GotoDefinitionResponse::Array(vec![
                at(3),
                at(1),
                at(3)
            ])),
            [at(3), at(1)]
        );
        let link = lsp::LocationLink {
            origin_selection_range: None,
            target_uri: uri.clone(),
            target_range: range(4, 0),
            target_selection_range: range(5, 2),
        };
        assert_eq!(
            locations(lsp::GotoDefinitionResponse::Link(vec![link])),
            [lsp::Location {
                uri: uri.clone(),
                range: range(5, 2)
            }]
        );
    }

    /// A tree of symbols is listed from the top of the file down, each
    /// under the one it is in, at its name; a flat list says what each is
    /// in.
    #[test]
    fn symbols_of_a_file() {
        let uri: lsp::Uri = "file:///p/a.rs".parse().unwrap();
        #[allow(deprecated)]
        let symbol = |name: &str, kind, line, children| lsp::DocumentSymbol {
            name: name.into(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: range(line, 0),
            selection_range: range(line, 4),
            children,
        };
        let tree = vec![
            symbol("main", lsp::SymbolKind::FUNCTION, 9, None),
            symbol(
                "Point",
                lsp::SymbolKind::STRUCT,
                1,
                Some(vec![
                    symbol("y", lsp::SymbolKind::FIELD, 3, None),
                    symbol("x", lsp::SymbolKind::FIELD, 2, None),
                ]),
            ),
        ];
        let places = symbol_places(lsp::DocumentSymbolResponse::Nested(tree), &uri);
        let listed: Vec<_> = places
            .iter()
            .map(|p| (p.label.as_deref().unwrap(), p.location.range.start.line))
            .collect();
        assert_eq!(
            listed,
            [
                ("struct Point", 1),
                ("  field x", 2),
                ("  field y", 3),
                ("function main", 9),
            ]
        );
        assert_eq!(places[0].location.range, range(1, 4));

        #[allow(deprecated)]
        let flat = |name: &str, container: Option<&str>, line| lsp::SymbolInformation {
            name: name.into(),
            kind: lsp::SymbolKind::METHOD,
            tags: None,
            deprecated: None,
            location: lsp::Location {
                uri: uri.clone(),
                range: range(line, 0),
            },
            container_name: container.map(Into::into),
        };
        let places = symbol_places(
            lsp::DocumentSymbolResponse::Flat(vec![
                flat("draw", Some("Shape"), 7),
                flat("new", None, 2),
            ]),
            &uri,
        );
        let labels: Vec<_> = places.iter().map(|p| p.label.clone().unwrap()).collect();
        assert_eq!(labels, ["method new", "method Shape.draw"]);
    }

    /// Each item asked for gets its section of the settings, or all of them.
    #[test]
    fn answers_for_the_configuration() {
        let settings = json!({ "python": { "analysis": { "strict": true } }, "validate": "on" });
        let params = json!({ "items": [
            { "section": "python.analysis" },
            { "scopeUri": "file:///p/a.js" },
            { "section": "nothing" },
        ] });
        assert_eq!(
            configuration(&settings, &params),
            json!([{ "strict": true }, settings, null])
        );
    }

    /// Every server set up for a file's language serves it, one of each
    /// group of med's own, in the order they were set up; one turned off
    /// does not.
    #[cfg(unix)]
    #[gpui::test]
    fn a_file_has_every_server_of_its_language(cx: &mut gpui::TestAppContext) {
        use crate::lsp_config::LspConfig;
        use gpui::AppContext as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("med-lsp-many-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for program in [
            "typescript-language-server",
            "eslint-ls",
            "tailwind-ls",
            "off-ls",
        ] {
            let server = dir.join(program);
            std::fs::write(&server, "#!/bin/sh\nexec cat >/dev/null\n").unwrap();
            std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:/usr/bin:/bin", dir.display());
        let cell = EnvCell::ready(ProjectEnv::from_vars([("PATH", path.as_str())]));
        cx.update(|cx| {
            crate::script::init(cx);
            let program = r#"
                (lsp-server "eslint" :command "eslint-ls" :languages '("typescript"))
                (lsp-server "tailwind" :command "tailwind-ls" :languages '("typescript" "css"))
                (lsp-server "off" :command "off-ls" :languages '("typescript") :enabled false)
                (lsp-server "missing" :command "not-there-ls" :languages '("typescript"))
            "#;
            crate::script::eval(program, cx).unwrap();
            assert!(cx.has_global::<LspConfig>());
        });

        let file = dir.join("a.ts");
        let lang = crate::syntax::lang_for_path(&file).unwrap();
        let store = cx.new(|_| LspStore::with_env(dir.clone(), true, cell));
        store.update(cx, |store, cx| {
            store.open(&file, lang, "let a = 1".into(), cx)
        });
        store.read_with(cx, |store, _| {
            assert_eq!(
                store.status(&file).as_deref(),
                Some("typescript-language-server: starting, eslint: starting, tailwind: starting")
            );
            assert!(store.failed.is_empty());
        });

        // What each server says of a file is its own, and all are shown.
        let diagnostic = |message: &str| lsp::Diagnostic {
            message: message.into(),
            ..Default::default()
        };
        store.update(cx, |store, _| {
            store.publish("eslint", file.clone(), vec![diagnostic("unused")]);
            store.publish(
                "typescript-language-server",
                file.clone(),
                vec![diagnostic("type")],
            );
            store.publish("eslint", file.clone(), vec![diagnostic("semicolon")]);
            let messages: Vec<_> = store
                .diagnostics(&file)
                .iter()
                .map(|d| &d.message)
                .collect();
            assert_eq!(messages, ["semicolon", "type"]);
        });
        store.update(cx, |store, cx| store.restart(cx));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
