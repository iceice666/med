//! A minimal LSP client transport: spawn a language server and speak
//! JSON-RPC with it over stdio. No UI or executor dependencies; replies and
//! server messages come back through `futures` channels.

use anyhow::{Result, anyhow};
use futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub use lsp_types;

#[derive(Clone, Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// A message initiated by the server.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// Must be answered with [`Client::respond`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The server closed its stdout; no more messages will arrive.
    Exited,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

pub struct Client {
    outgoing: std::sync::mpsc::Sender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicI64,
    child: Mutex<Child>,
}

fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Read one framed message. `Ok(None)` on a clean EOF.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn dispatch(message: Value, pending: &Pending, incoming: &mpsc::UnboundedSender<Incoming>) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        (Some(method), Some(id)) => {
            let _ = incoming.unbounded_send(Incoming::Request {
                id,
                method: method.to_string(),
                params: message.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        (Some(method), None) => {
            let _ = incoming.unbounded_send(Incoming::Notification {
                method: method.to_string(),
                params: message.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(reply) = pending.lock().unwrap().remove(&id) else {
                return;
            };
            let result = match message.get("error") {
                Some(error) => Err(RpcError {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = reply.send(result);
        }
        (None, None) => {}
    }
}

impl Client {
    /// Start `program` in `cwd`. Server-initiated messages arrive on the
    /// returned receiver, ending with [`Incoming::Exited`].
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> io::Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (outgoing, outgoing_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();

        thread::Builder::new()
            .name(format!("lsp-write-{program}"))
            .spawn(move || {
                for bytes in outgoing_rx {
                    if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
            })?;

        let reader_pending = pending.clone();
        thread::Builder::new()
            .name(format!("lsp-read-{program}"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                while let Ok(Some(message)) = read_message(&mut reader) {
                    dispatch(message, &reader_pending, &incoming_tx);
                }
                // Fail every in-flight request.
                reader_pending.lock().unwrap().clear();
                let _ = incoming_tx.unbounded_send(Incoming::Exited);
            })?;

        // Servers log to stderr; drain it so they never block on a full pipe.
        thread::Builder::new()
            .name(format!("lsp-stderr-{program}"))
            .spawn(move || {
                let mut sink = [0u8; 4096];
                let mut stderr = stderr;
                while matches!(stderr.read(&mut sink), Ok(n) if n > 0) {}
            })?;

        Ok((
            Self {
                outgoing,
                pending,
                next_id: AtomicI64::new(1),
                child: Mutex::new(child),
            },
            incoming_rx,
        ))
    }

    fn send(&self, message: Value) {
        let _ = self.outgoing.send(encode(&message));
    }

    pub fn request_raw(
        &self,
        method: &str,
        params: Value,
    ) -> oneshot::Receiver<Result<Value, RpcError>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        rx
    }

    /// Send a typed request; the future resolves with the server's reply.
    pub fn request<R>(&self, params: R::Params) -> impl Future<Output = Result<R::Result>> + use<R>
    where
        R: lsp_types::request::Request,
    {
        let reply = serde_json::to_value(params).map(|params| self.request_raw(R::METHOD, params));
        async move {
            let value = reply?
                .await
                .map_err(|_| anyhow!("language server exited"))??;
            Ok(serde_json::from_value(value)?)
        }
    }

    pub fn notify<N>(&self, params: N::Params)
    where
        N: lsp_types::notification::Notification,
    {
        if let Ok(params) = serde_json::to_value(params) {
            self.send(json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params }));
        }
    }

    /// Answer a server-initiated request.
    pub fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": error.code, "message": error.message },
            }),
        };
        self.send(message);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::executor::block_on;

    #[test]
    fn framing_roundtrip() {
        let a = json!({ "jsonrpc": "2.0", "method": "x", "params": { "s": "好" } });
        let b = json!({ "jsonrpc": "2.0", "id": 1, "result": null });
        let mut bytes = encode(&a);
        bytes.extend(encode(&b));
        let mut reader = BufReader::new(bytes.as_slice());
        assert_eq!(read_message(&mut reader).unwrap(), Some(a));
        assert_eq!(read_message(&mut reader).unwrap(), Some(b));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    /// `cat` echoes our own frames back, which is enough to exercise the
    /// whole pipeline: a request we send comes back as a server request.
    #[test]
    fn talks_to_a_process() {
        let (client, mut incoming) = Client::spawn("cat", &[], Path::new("/")).unwrap();
        client.notify::<lsp_types::notification::Exit>(());
        match block_on(incoming.next()).unwrap() {
            Incoming::Notification { method, .. } => assert_eq!(method, "exit"),
            other => panic!("unexpected {other:?}"),
        }
        let _reply = client.request_raw("ping", json!({ "n": 1 }));
        match block_on(incoming.next()).unwrap() {
            Incoming::Request { id, method, params } => {
                assert_eq!(method, "ping");
                assert_eq!(params, json!({ "n": 1 }));
                assert_eq!(id, json!(1));
            }
            other => panic!("unexpected {other:?}"),
        }
        drop(client);
        assert!(matches!(block_on(incoming.next()), Some(Incoming::Exited)));
    }
}
