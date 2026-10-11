//! The ACP (Agent Client Protocol) stdio server.
//!
//! Hand-rolled over the stable ACP **v1** method set. The protocol is JSON-RPC
//! 2.0 with one message per line on stdin/stdout:
//!
//! * client → agent requests we implement: `initialize`, `session/new`,
//!   `session/prompt`; notification `session/cancel`.
//! * agent → client: `session/update` notifications (streamed text, thinking,
//!   tool calls) and one JSON-RPC response per request.
//!
//! We deliberately do **not** implement the client-side `fs/*` and `terminal/*`
//! methods: rpi already has native `read`/`write`/`edit`/`bash` tools, so the
//! agent runs its own tools in `session/new.cwd` instead of borrowing the
//! editor's. That keeps the surface small and the behavior identical to the
//! terminal agent.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch, Mutex};

use crate::session::Session;

/// ACP protocol major version we speak. Stable v1.
pub const PROTOCOL_VERSION: u64 = 1;

/// One block of a `session/prompt` message. Modeled types are the ones we act
/// on; every other ACP block (`image`, `audio`, …) lands in [`ContentBlock::Other`]
/// and is ignored, matching our advertised `promptCapabilities`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ResourceLink {
        #[serde(default)]
        name: Option<String>,
        uri: String,
    },
    Resource {
        resource: EmbeddedResource,
    },
    #[serde(other)]
    Other,
}

/// An embedded file in a `resource` block (`promptCapabilities.embeddedContext`).
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// Serialized access to the process stdout — the single ACP response stream.
/// Shared by the request dispatcher and every in-flight prompt turn, so writes
/// are serialized and each message is exactly one flushed line.
pub struct Writer {
    out: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    /// In-flight agent → client requests (e.g. `session/request_permission`),
    /// keyed by the JSON-RPC id we sent. The client's response (an object with
    /// `id` and no `method`) resolves the matching oneshot.
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    closed: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
}

impl Writer {
    pub fn new() -> Self {
        Self {
            out: Mutex::new(Box::new(tokio::io::stdout())),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }
    }

    async fn write(&self, value: &Value) -> std::io::Result<()> {
        let mut line = value.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        out.write_all(line.as_bytes()).await?;
        out.flush().await
    }

    /// Answer a JSON-RPC request.
    pub async fn respond(&self, id: &Value, result: Value) {
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    /// Answer a JSON-RPC request with an error.
    pub async fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let _ = self
            .write(&json!({
                "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message},
            }))
            .await;
    }

    /// Send a JSON-RPC notification (no `id`).
    pub async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    /// Send an agent → client request and await its response. Resolves to the
    /// `result` object, or `Null` if the connection closes first.
    pub async fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Value::Null;
            }
            pending.insert(id, tx);
        }
        // A cancelled dialog drops its pending request even when the client never replies.
        let _request = PendingRequest { writer: self, id };
        if self
            .write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
            .is_err()
        {
            self.close();
            return Value::Null;
        }
        rx.await.unwrap_or(Value::Null)
    }

    pub fn close(&self) {
        let mut pending = self.pending.lock().unwrap();
        self.closed.store(true, Ordering::Release);
        pending.clear();
    }

    #[cfg(test)]
    pub(crate) fn with_output(out: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            out: Mutex::new(Box::new(out)),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Route a client response (an object with `id` and no `method`) to the
    /// request that produced it.
    pub(crate) async fn resolve(&self, id: u64, result: Value) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }
}

struct PendingRequest<'a> {
    writer: &'a Writer,
    id: u64,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.writer.pending.lock().unwrap().remove(&self.id);
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

/// The `initialize` result. Advertises stable ACP v1 and the content we accept.
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "agentCapabilities": {
            // We cannot replay rpi session history as ACP updates yet.
            "loadSession": false,
            "promptCapabilities": {
                // `--mode rpc` takes a single text prompt; no multimodal input.
                "image": false,
                "audio": false,
                // Editor context (open files / selections) arrives as `resource`
                // blocks, which we inline into the prompt text.
                "embeddedContext": true,
            },
        },
        "agentInfo": {
            "name": "rpi",
            "title": "rpi",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "authMethods": [],
    })
}

/// Run the ACP server until the client closes stdin, then return an exit code.
///
/// The owner reports the returned code to the host's stdio lifecycle. The
/// stop receiver also lets SessionShutdown end an idle server without input.
pub async fn run(mut stop: watch::Receiver<bool>) -> i32 {
    let writer = Arc::new(Writer::new());
    let sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut next_session: u64 = 0;

    let mut input = rpi_plugin_sdk::stdio::StdinLines::default();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut exit_code = 0;
    'input: loop {
        let line = loop {
            if *stop.borrow() {
                break 'input;
            }
            match input.poll_line() {
                Ok(rpi_plugin_sdk::stdio::InputLine::Line(line)) => break line,
                Ok(rpi_plugin_sdk::stdio::InputLine::Eof) => break 'input,
                Ok(rpi_plugin_sdk::stdio::InputLine::Pending) => {
                    tokio::select! {
                        biased;
                        _ = stop.changed() => break 'input,
                        _ = tick.tick() => {},
                    }
                }
                Err(error) => {
                    eprintln!("[rpi-acp] stdin read error: {error}");
                    exit_code = 2;
                    break 'input;
                }
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[rpi-acp] invalid json from client: {error}");
                continue;
            }
        };

        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").cloned();

        match (id, method.as_deref()) {
            (Some(id), Some("initialize")) => writer.respond(&id, initialize_result()).await,
            // No auth methods are advertised, but tolerate a stray call.
            (Some(id), Some("authenticate")) => writer.respond(&id, json!({})).await,
            (Some(id), Some("session/new")) => {
                let cwd = message
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                next_session += 1;
                let session_id = format!("rpi-{}-{}", std::process::id(), next_session);
                match Session::spawn(session_id.clone(), PathBuf::from(&cwd)) {
                    Ok(session) => {
                        sessions.lock().await.insert(session_id.clone(), session);
                        writer.respond(&id, json!({"sessionId": session_id})).await;
                    }
                    Err(error) => writer.respond_error(&id, -32000, &error).await,
                }
            }
            (Some(id), Some("session/prompt")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let blocks: Vec<ContentBlock> = message
                    .pointer("/params/prompt")
                    .and_then(Value::as_array)
                    .map(|array| {
                        array
                            .iter()
                            .filter_map(|value| serde_json::from_value(value.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let session = sessions.lock().await.get(&session_id).cloned();
                match session {
                    Some(session) => {
                        let writer = Arc::clone(&writer);
                        tokio::spawn(async move {
                            let stop_reason = session.prompt(&writer, &session_id, &blocks).await;
                            writer
                                .respond(&id, json!({"stopReason": stop_reason}))
                                .await;
                        });
                    }
                    None => writer.respond_error(&id, -32602, "unknown sessionId").await,
                }
            }
            (Some(id), Some("session/load")) | (Some(id), Some("session/resume")) => {
                writer
                    .respond_error(&id, -32601, "session resumption is not supported")
                    .await
            }
            (Some(id), Some(other)) => {
                writer
                    .respond_error(&id, -32601, &format!("method not found: {other}"))
                    .await
            }
            // A client response (an object with `id` and no `method`) answers an
            // agent → client request, e.g. the `session/request_permission` we
            // sent for a plugin's UI dialog. Route it to the waiting caller.
            (Some(id), None) => {
                if let Some(numeric_id) = id.as_u64() {
                    let result = message.get("result").cloned().unwrap_or(Value::Null);
                    writer.resolve(numeric_id, result).await;
                }
            }
            (None, Some("session/cancel")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let session = sessions.lock().await.get(&session_id).cloned();
                if let Some(session) = session {
                    let _ = session.abort().await;
                }
            }
            (None, _) => {}
        }
    }

    // Resolve pending client requests before tearing down every child.
    writer.close();
    // Client closed stdin or host requested shutdown.
    for (_, session) in sessions.lock().await.drain() {
        session.kill().await;
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disconnect_resolves_pending_requests_and_rejects_new_ones() {
        let (output, peer) = tokio::io::duplex(1024);
        let writer = Arc::new(Writer::with_output(output));
        let request = tokio::spawn({
            let writer = writer.clone();
            async move { writer.request("test", json!({})).await }
        });
        let mut lines = BufReader::new(peer).lines();
        lines.next_line().await.unwrap().unwrap();
        assert_eq!(writer.pending_count(), 1);
        writer.close();
        assert_eq!(request.await.unwrap(), Value::Null);
        assert_eq!(writer.pending_count(), 0);
        assert_eq!(writer.request("test", json!({})).await, Value::Null);
    }

    #[test]
    fn initialize_advertises_v1_and_embedded_context() {
        let result = initialize_result();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["agentCapabilities"]["loadSession"], false);
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            true
        );
        assert_eq!(result["agentInfo"]["name"], "rpi");
    }

    #[test]
    fn content_blocks_parse_text_link_and_resource() {
        let text: ContentBlock =
            serde_json::from_value(json!({"type":"text","text":"hi"})).unwrap();
        assert!(matches!(text, ContentBlock::Text { .. }));

        let link: ContentBlock = serde_json::from_value(
            json!({"type":"resource_link","uri":"file:///a.rs","name":"a.rs"}),
        )
        .unwrap();
        assert!(matches!(link, ContentBlock::ResourceLink { .. }));

        let resource: ContentBlock = serde_json::from_value(json!({
            "type": "resource",
            "resource": {"uri": "file:///a.rs", "mimeType": "text/rust", "text": "fn main(){}"}
        }))
        .unwrap();
        match resource {
            ContentBlock::Resource { resource } => {
                assert_eq!(resource.uri, "file:///a.rs");
                assert_eq!(resource.text.as_deref(), Some("fn main(){}"));
            }
            _ => panic!("expected resource block"),
        }
    }

    #[test]
    fn unknown_content_block_is_ignored() {
        let image: ContentBlock =
            serde_json::from_value(json!({"type":"image","data":"...","mimeType":"image/png"}))
                .unwrap();
        assert!(matches!(image, ContentBlock::Other));
    }
}
