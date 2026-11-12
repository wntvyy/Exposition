//! A small Language Server Protocol client over an interactive job's stdio.
//!
//! - **Framing.** `Content-Length` headers; bounded header and body sizes so a
//!   misbehaving server cannot exhaust memory. A framing error ends the session
//!   (it is treated like a crash and the server is restarted).
//! - **Requests.** Integer ids, correlated by a pending map. Every request can
//!   be cancelled (`$/cancelRequest`), and dropping a pending request before it
//!   completes cancels it on the server.
//! - **Server requests** (`client/registerCapability`, refresh requests, ...)
//!   are answered immediately; unknown ones with `MethodNotFound`.
//! - **Notifications** are forwarded, in order, to the session owner.
//! - **End of stream** fails every pending request with [`LspError::Closed`]
//!   and delivers [`Incoming::Closed`].
//!
//! Server output is data: it is parsed as JSON and never interpreted further
//! here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_execution::{InteractiveInput, InteractiveOutput};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// Largest accepted message body (Lean diagnostics for huge files stay far below).
pub const MAX_BODY: usize = 64 * 1024 * 1024;
const MAX_HEADER: usize = 8 * 1024;

/// JSON-RPC error code for a request the server cancelled.
pub const REQUEST_CANCELLED: i64 = -32800;
/// LSP `ContentModified`: the document changed while the request ran.
pub const CONTENT_MODIFIED: i64 = -32801;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LspError {
    #[error("the language server is not running")]
    Closed,
    #[error("the language server returned error {code}: {message}")]
    Response { code: i64, message: String },
    #[error("the request was cancelled")]
    Cancelled,
    #[error("the language server did not answer in time")]
    Timeout,
    #[error("invalid language server message: {0}")]
    Protocol(String),
}

impl LspError {
    /// The server abandoned the request because the document changed or the
    /// request was cancelled.
    pub fn is_superseded(&self) -> bool {
        matches!(self, LspError::Cancelled)
            || matches!(self, LspError::Response { code, .. } if *code == REQUEST_CANCELLED || *code == CONTENT_MODIFIED)
    }
}

/// Incremental decoder for `Content-Length` framed messages.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete message body, `Ok(None)` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, LspError> {
        let Some(end) = find(&self.buffer, b"\r\n\r\n") else {
            if self.buffer.len() > MAX_HEADER {
                return Err(LspError::Protocol("header section too long".into()));
            }
            return Ok(None);
        };
        if end > MAX_HEADER {
            return Err(LspError::Protocol("header section too long".into()));
        }
        let header =
            std::str::from_utf8(&self.buffer[..end]).map_err(|_| LspError::Protocol("header is not UTF-8".into()))?;
        let mut length = None;
        for line in header.split("\r\n") {
            let Some((name, value)) = line.split_once(':') else {
                return Err(LspError::Protocol(format!("malformed header line {line:?}")));
            };
            if name.trim().eq_ignore_ascii_case("content-length") {
                let parsed: usize =
                    value.trim().parse().map_err(|_| LspError::Protocol("invalid Content-Length".into()))?;
                length = Some(parsed);
            }
        }
        let length = length.ok_or_else(|| LspError::Protocol("missing Content-Length".into()))?;
        if length > MAX_BODY {
            return Err(LspError::Protocol(format!("message of {length} bytes exceeds the limit")));
        }
        let start = end + 4;
        if self.buffer.len() < start + length {
            return Ok(None);
        }
        let body = self.buffer[start..start + length].to_vec();
        self.buffer.drain(..start + length);
        Ok(Some(body))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Frame one JSON-RPC message.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("JSON values always serialize");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Messages delivered to the session owner, in stream order.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// The server's stdout ended (or the stream became unreadable).
    Closed {
        reason: Option<String>,
    },
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>>;

/// A connected client. Cheap to share (`Arc`); all methods take `&self`.
pub struct LspClient {
    /// Ordered outgoing frames; a writer task forwards them to the job's stdin.
    out: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicI64,
    pending: Pending,
    closed: Arc<AtomicBool>,
}

/// A request in flight. Await [`PendingRequest::response`]; drop it (or call
/// [`PendingRequest::cancel`]) to cancel the request on the server.
pub struct PendingRequest {
    id: i64,
    rx: Option<oneshot::Receiver<Result<Value, LspError>>>,
    client: Arc<LspClient>,
}

impl PendingRequest {
    pub fn id(&self) -> i64 {
        self.id
    }

    /// Wait for the response, cancelling the request when `timeout` passes.
    pub async fn response(mut self, timeout: Duration) -> Result<Value, LspError> {
        let rx = self.rx.take().expect("response awaited once");
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::Closed),
            Err(_) => {
                self.client.cancel(self.id);
                Err(LspError::Timeout)
            }
        }
    }

    pub fn cancel(mut self) {
        self.rx = None;
        self.client.cancel(self.id);
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        // Still waiting: the caller lost interest, so the server can stop working on it.
        if self.rx.is_some() {
            self.client.cancel(self.id);
        }
    }
}

impl LspClient {
    /// Start reading `output`; returns the client and the ordered stream of
    /// notifications. Must be called inside a Tokio runtime.
    pub fn start(
        input: InteractiveInput,
        output: InteractiveOutput,
    ) -> (Arc<LspClient>, mpsc::UnboundedReceiver<Incoming>) {
        let (out, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        let client = Arc::new(LspClient {
            out: out.clone(),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            closed: Arc::new(AtomicBool::new(false)),
        });
        tokio::spawn(async move {
            while let Some(frame) = queue.recv().await {
                if input.write(frame).await.is_err() {
                    break;
                }
            }
        });
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(output, out, client.pending.clone(), client.closed.clone(), tx));
        (client, rx)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Send a request; the response is awaited through the returned handle.
    pub fn request(self: &Arc<Self>, method: &str, params: Value) -> PendingRequest {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if self.is_closed() {
            let _ = tx.send(Err(LspError::Closed));
        } else {
            self.pending.lock().expect("pending").insert(id, tx);
            let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            if self.out.send(encode(&message)).is_err()
                && let Some(tx) = self.pending.lock().expect("pending").remove(&id)
            {
                let _ = tx.send(Err(LspError::Closed));
            }
        }
        PendingRequest { id, rx: Some(rx), client: self.clone() }
    }

    /// Convenience: request and wait with a timeout.
    pub async fn call(self: &Arc<Self>, method: &str, params: Value, timeout: Duration) -> Result<Value, LspError> {
        self.request(method, params).response(timeout).await
    }

    /// Send a notification. Ordered with every other message from this client.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        if self.is_closed() {
            return Err(LspError::Closed);
        }
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.out.send(encode(&message)).map_err(|_| LspError::Closed)
    }

    /// Cancel a request: the waiter gets [`LspError::Cancelled`] and the server
    /// is told to stop (it may still answer; the answer is ignored).
    pub fn cancel(&self, id: i64) {
        let removed = self.pending.lock().expect("pending").remove(&id);
        if let Some(tx) = removed {
            let _ = tx.send(Err(LspError::Cancelled));
            let _ = self.notify("$/cancelRequest", json!({ "id": id }));
        }
    }

    /// Requests still waiting for an answer.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().expect("pending").len()
    }
}

async fn read_loop(
    mut output: InteractiveOutput,
    input: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    closed: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Incoming>,
) {
    let mut decoder = FrameDecoder::default();
    let reason = 'read: loop {
        let Some(bytes) = output.recv().await else { break 'read None };
        decoder.push(&bytes);
        loop {
            let body = match decoder.next_frame() {
                Ok(Some(body)) => body,
                Ok(None) => break,
                Err(e) => break 'read Some(e.to_string()),
            };
            let message: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => break 'read Some(format!("invalid JSON from the language server: {e}")),
            };
            dispatch(message, &input, &pending, &tx);
        }
    };
    closed.store(true, Ordering::SeqCst);
    let waiters: Vec<_> = pending.lock().expect("pending").drain().map(|(_, w)| w).collect();
    for waiter in waiters {
        let _ = waiter.send(Err(LspError::Closed));
    }
    let _ = tx.send(Incoming::Closed { reason });
}

fn dispatch(
    message: Value,
    input: &mpsc::UnboundedSender<Vec<u8>>,
    pending: &Pending,
    tx: &mpsc::UnboundedSender<Incoming>,
) {
    let method = message.get("method").and_then(Value::as_str).map(str::to_owned);
    let id = message.get("id").cloned();
    match (method, id) {
        // Response to one of our requests.
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(waiter) = pending.lock().expect("pending").remove(&id) else { return };
            let result = if let Some(error) = message.get("error") {
                Err(LspError::Response {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error.get("message").and_then(Value::as_str).unwrap_or("").chars().take(2000).collect(),
                })
            } else {
                Ok(message.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = waiter.send(result);
        }
        // Request from the server: we register no dynamic capabilities and have
        // nothing to refresh, so acknowledging is correct.
        (Some(method), Some(id)) => {
            let known = matches!(
                method.as_str(),
                "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create"
                    | "workspace/inlayHint/refresh"
                    | "workspace/semanticTokens/refresh"
                    | "workspace/codeLens/refresh"
                    | "workspace/diagnostic/refresh"
            );
            let reply = if known {
                json!({ "jsonrpc": "2.0", "id": id, "result": null })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not supported by Exposition" } })
            };
            let _ = input.send(encode(&reply));
        }
        (Some(method), None) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let _ = tx.send(Incoming::Notification { method, params });
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_and_batched_frames() {
        let a = encode(&json!({"jsonrpc":"2.0","method":"a","params":{"x":"αβ"}}));
        let b = encode(&json!({"jsonrpc":"2.0","id":3,"result":null}));
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut decoder = FrameDecoder::default();
        // Byte by byte.
        let mut frames = Vec::new();
        for byte in &all {
            decoder.push(&[*byte]);
            while let Some(frame) = decoder.next_frame().unwrap() {
                frames.push(frame);
            }
        }
        assert_eq!(frames.len(), 2);
        let first: Value = serde_json::from_slice(&frames[0]).unwrap();
        assert_eq!(first["params"]["x"], "αβ", "Content-Length counts bytes, not characters");
        // All at once, with extra headers.
        let mut decoder = FrameDecoder::default();
        decoder.push(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}");
        assert_eq!(decoder.next_frame().unwrap().unwrap(), b"{}");
        assert!(decoder.next_frame().unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_and_oversized_frames() {
        let mut d = FrameDecoder::default();
        d.push(b"Content-Length: abc\r\n\r\n");
        assert!(d.next_frame().is_err());
        let mut d = FrameDecoder::default();
        d.push(format!("Content-Length: {}\r\n\r\n", MAX_BODY + 1).as_bytes());
        assert!(d.next_frame().is_err());
        let mut d = FrameDecoder::default();
        d.push(b"X-Other: 1\r\n\r\n{}");
        assert!(d.next_frame().is_err(), "missing length");
        let mut d = FrameDecoder::default();
        d.push(&vec![b'a'; MAX_HEADER + 1]);
        assert!(d.next_frame().is_err(), "unbounded header");
    }
}
