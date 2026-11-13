//! Anthropic Claude Messages API adapter (raw HTTPS; there is no official
//! Rust SDK).
//!
//! Request shape (per the Messages API documentation):
//! - `POST /v1/messages` with `x-api-key`, `anthropic-version: 2026-10-21`, and
//!   `anthropic-beta: server-side-fallback-2026-07-01`.
//! - Streaming (`stream: true`) with `max_tokens` up to 64 000.
//! - Adaptive thinking (`{type: "adaptive", display: "summarized"}`; disabling
//!   thinking or budgets are rejected on Claude Opus 5.5) and an explicit
//!   `output_config.effort` (the model default would otherwise be `medium`).
//! - `tool_choice: auto` with strict tools and `eager_input_streaming: true`
//!   (forced tool choice is rejected); streamed tool inputs are validated by the
//!   task loop before anything runs.
//! - `fallbacks: "default"`: on a policy refusal the API retries on its
//!   recommended fallback model; the switch is surfaced to the user.
//! - Prompt caching: an explicit breakpoint on the stable system prompt (which
//!   caches the tool list too) plus top-level automatic caching for the
//!   growing conversation.
//!
//! Retries: 429 (honoring `retry-after`), 5xx, 529 overloaded, connection
//! failures, and retryable stream `error` events, with capped exponential
//! backoff. Cancellation aborts the request and stream immediately. The API key
//! never appears in logs, errors, or events.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::sse::{MessageAssembler, SseDecoder, bounded};
use super::{EventSink, Provider, ProviderError, ProviderRequest, ProviderResponse, StreamEvent};
use crate::credentials::Secret;
use crate::model::{Effort, ProviderKind};

pub const API_BASE: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Per-request output ceiling for streamed agent turns.
pub const MAX_TOKENS_PER_TURN: u32 = 64_000;

/// Whether `model` accepts the server-side `fallbacks` parameter (Opus 5 and
/// later Opus, Fable/Mythos 5.x, Sonnet 5.5). Other models (e.g. Haiku 5.5)
/// reject it, so it is only sent where it is supported.
pub fn supports_server_fallbacks(model: &str) -> bool {
    ["claude-opus-5", "claude-fable-5", "claude-mythos-5", "claude-sonnet-5-5"].iter().any(|p| model.starts_with(p))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 4, base_delay: Duration::from_millis(1000), max_delay: Duration::from_secs(30) }
    }
}

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Secret,
    model: String,
    effort: Effort,
    retry: RetryPolicy,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(api_key: Secret, model: String, effort: Effort) -> Result<Self, ProviderError> {
        Self::with_base_url(API_BASE, api_key, model, effort, RetryPolicy::default())
    }

    /// `base_url` is fixed to [`API_BASE`] in the application; tests point it
    /// at a local server.
    pub fn with_base_url(
        base_url: &str,
        api_key: Secret,
        model: String,
        effort: Effort,
        retry: RetryPolicy,
    ) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            // Idle limit between stream chunks (the API sends periodic pings).
            .read_timeout(Duration::from_secs(300))
            .user_agent(concat!("Exposition/", env!("CARGO_PKG_VERSION")))
            // Never follow a redirect: it would re-send the API key header and
            // the request body (document context) to wherever it points.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Network(format!("cannot create HTTP client: {e}")))?;
        Ok(Self { client, base_url: base_url.trim_end_matches('/').to_owned(), api_key, model, effort, retry })
    }

    /// The JSON body for one streamed request.
    pub fn request_body(&self, request: &ProviderRequest) -> Value {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                    "strict": tool.strict,
                    "eager_input_streaming": true,
                })
            })
            .collect();
        let messages: Vec<Value> =
            request.messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": request.max_tokens.min(MAX_TOKENS_PER_TURN),
            "stream": true,
            "system": [{"type": "text", "text": request.system, "cache_control": {"type": "ephemeral"}}],
            "tools": tools,
            "tool_choice": {"type": "auto"},
            "thinking": {"type": "adaptive", "display": "summarized"},
            "output_config": {"effort": self.effort.as_str()},
            "cache_control": {"type": "ephemeral"},
            "messages": messages,
        });
        if supports_server_fallbacks(&self.model) {
            body["fallbacks"] = json!("default");
        }
        body
    }

    fn redact(&self, text: &str) -> String {
        let key = self.api_key.expose();
        let redacted = if key.is_empty() { text.to_owned() } else { text.replace(key, "<redacted>") };
        bounded(&redacted)
    }

    fn backoff(&self, attempt: u32, error: &ProviderError) -> Duration {
        if let ProviderError::RateLimited { retry_after_ms: Some(ms), .. } = error {
            return Duration::from_millis(*ms).min(Duration::from_secs(120));
        }
        let exp = self.retry.base_delay.saturating_mul(1u32 << attempt.min(10));
        // Deterministic jitter in [0, base/2) without a RNG dependency.
        let jitter_ms = (u64::from(std::process::id()) * 7919 + u64::from(attempt) * 104_729)
            % (self.retry.base_delay.as_millis() as u64 / 2 + 1);
        (exp + Duration::from_millis(jitter_ms)).min(self.retry.max_delay)
    }

    async fn attempt(
        &self,
        body: &[u8],
        sink: EventSink<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderResponse, (ProviderError, bool)> {
        let mut builder = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", API_VERSION);
        if supports_server_fallbacks(&self.model) {
            builder = builder.header("anthropic-beta", FALLBACK_BETA);
        }
        let send = builder
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body.to_vec())
            .send();
        let response = tokio::select! {
            r = send => r.map_err(|e| (ProviderError::Network(self.redact(&describe_reqwest(&e))), false))?,
            _ = cancel.cancelled() => return Err((ProviderError::Canceled, false)),
        };
        let status = response.status();
        if !status.is_success() {
            let retry_after_ms = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|secs| (secs.max(0.0) * 1000.0) as u64);
            let text = tokio::select! {
                t = response.text() => t.unwrap_or_default(),
                _ = cancel.cancelled() => return Err((ProviderError::Canceled, false)),
            };
            return Err((self.http_error(status.as_u16(), &text, retry_after_ms), false));
        }

        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::new();
        let mut assembler = MessageAssembler::new();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                _ = cancel.cancelled() => return Err((ProviderError::Canceled, assembler.emitted)),
            };
            match next {
                Some(Ok(chunk)) => {
                    for event in decoder.push(&chunk) {
                        assembler.handle(&event, sink).map_err(|e| {
                            let emitted = assembler.emitted;
                            (self.redact_error(e), emitted)
                        })?;
                    }
                    if assembler.is_complete() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    let emitted = assembler.emitted;
                    return Err((ProviderError::Network(self.redact(&describe_reqwest(&e))), emitted));
                }
                None => {
                    if let Some(event) = decoder.finish() {
                        let emitted = assembler.emitted;
                        assembler.handle(&event, sink).map_err(|e| (self.redact_error(e), emitted))?;
                    }
                    break;
                }
            }
        }
        let emitted = assembler.emitted;
        assembler.finish().map_err(|e| (e, emitted))
    }

    fn redact_error(&self, error: ProviderError) -> ProviderError {
        match error {
            ProviderError::Authentication(m) => ProviderError::Authentication(self.redact(&m)),
            ProviderError::InvalidRequest(m) => ProviderError::InvalidRequest(self.redact(&m)),
            ProviderError::Overloaded(m) => ProviderError::Overloaded(self.redact(&m)),
            ProviderError::Server { status, message } => {
                ProviderError::Server { status, message: self.redact(&message) }
            }
            ProviderError::RateLimited { retry_after_ms, message } => {
                ProviderError::RateLimited { retry_after_ms, message: self.redact(&message) }
            }
            other => other,
        }
    }

    fn http_error(&self, status: u16, body: &str, retry_after_ms: Option<u64>) -> ProviderError {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let message = parsed
            .as_ref()
            .and_then(|v| v.pointer("/error/message").and_then(Value::as_str))
            .map(|m| self.redact(m))
            .unwrap_or_else(|| format!("HTTP {status}"));
        match status {
            401 => ProviderError::Authentication(message),
            403 => ProviderError::Authentication(format!("permission denied: {message}")),
            429 => ProviderError::RateLimited { retry_after_ms, message },
            529 => ProviderError::Overloaded(message),
            300..=399 => ProviderError::InvalidRequest(format!(
                "the provider answered with a redirect (HTTP {status}); Exposition does not follow redirects, so \
                 nothing was sent to another address"
            )),
            400 | 402 | 404 | 413 | 422 => ProviderError::InvalidRequest(message),
            500..=599 => ProviderError::Server { status, message },
            _ => ProviderError::InvalidRequest(format!("unexpected HTTP {status}: {message}")),
        }
    }
}

fn describe_reqwest(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "the connection timed out".into()
    } else if error.is_connect() {
        "could not connect to the provider".into()
    } else if error.is_body() || error.is_decode() {
        "the connection was interrupted while streaming".into()
    } else {
        "the request failed".into()
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        sink: EventSink<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = serde_json::to_vec(&self.request_body(request))
            .map_err(|e| ProviderError::Protocol(format!("cannot encode request: {e}")))?;
        let mut attempt = 0u32;
        loop {
            match self.attempt(&body, sink, cancel).await {
                Ok(response) => return Ok(response),
                Err((error, emitted)) => {
                    if !error.is_retryable() || attempt >= self.retry.max_retries {
                        return Err(error);
                    }
                    let delay = self.backoff(attempt, &error);
                    tracing::warn!(attempt, delay_ms = delay.as_millis() as u64, "retrying provider request: {error}");
                    if emitted {
                        sink(StreamEvent::Reset { reason: format!("{error}; retrying") });
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.cancelled() => return Err(ProviderError::Canceled),
                    }
                    attempt += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::provider::{ProviderMessage, ToolDefinition};

    const KEY: &str = "sk-ant-test-0123456789abcdef";

    fn request() -> ProviderRequest {
        ProviderRequest {
            system: "system prompt".into(),
            tools: vec![ToolDefinition {
                name: "read_file",
                description: "Read a file",
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false}),
                strict: true,
            }],
            messages: vec![ProviderMessage {
                role: "user".into(),
                content: vec![json!({"type": "text", "text": "hi"})],
            }],
            max_tokens: 100_000,
        }
    }

    #[test]
    fn request_body_follows_documented_shape() {
        let provider =
            AnthropicProvider::new(Secret::new(KEY.to_owned()), "claude-opus-5-5".into(), Effort::High).unwrap();
        let body = provider.request_body(&request());
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["max_tokens"], 64_000);
        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(body["output_config"], json!({"effort": "high"}));
        assert_eq!(body["tool_choice"], json!({"type": "auto"}));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);
        assert!(body.get("temperature").is_none());
        let haiku =
            AnthropicProvider::new(Secret::new(KEY.to_owned()), "claude-haiku-5-5".into(), Effort::Low).unwrap();
        assert!(haiku.request_body(&request()).get("fallbacks").is_none());
        assert!(supports_server_fallbacks("claude-opus-5-5") && supports_server_fallbacks("claude-sonnet-5-5"));
        assert!(!body.to_string().contains(KEY));
        assert!(!format!("{provider:?}").contains(KEY));
    }

    /// Minimal HTTP/1.1 server: serves `responses` in order, records requests.
    async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let len = text[..head_end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= head_end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                seen_clone.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn sse_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    }

    const OK_STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    fn fast_retry() -> RetryPolicy {
        RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(10), max_delay: Duration::from_millis(50) }
    }

    #[tokio::test]
    async fn retries_overloaded_then_streams_with_documented_headers() {
        let overloaded = "HTTP/1.1 529 Overloaded\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: 74\r\n\r\n{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}";
        let (url, seen) = serve(vec![overloaded.to_owned(), sse_response(OK_STREAM)]).await;
        let provider = AnthropicProvider::with_base_url(
            &url,
            Secret::new(KEY.to_owned()),
            "claude-opus-5-5".into(),
            Effort::Medium,
            fast_retry(),
        )
        .unwrap();
        let events = Mutex::new(Vec::new());
        let sink = |e: StreamEvent| events.lock().unwrap().push(e);
        let response = provider.stream(&request(), &sink, &CancellationToken::new()).await.unwrap();
        assert_eq!(response.content, vec![json!({"type": "text", "text": "Hello"})]);
        assert_eq!(response.stop_reason, "end_turn");
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let head = requests[1].to_ascii_lowercase();
        assert!(head.starts_with("post /v1/messages "));
        assert!(head.contains("anthropic-version: 2026-10-21"));
        assert!(head.contains("anthropic-beta: server-side-fallback-2026-07-01"));
        assert!(head.contains(&format!("x-api-key: {KEY}")));
    }

    #[tokio::test]
    async fn does_not_retry_invalid_requests_and_redacts_the_key() {
        let body = format!(
            "{{\"type\":\"error\",\"error\":{{\"type\":\"invalid_request_error\",\"message\":\"bad key {KEY} in request\"}}}}"
        );
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, seen) = serve(vec![response]).await;
        let provider = AnthropicProvider::with_base_url(
            &url,
            Secret::new(KEY.to_owned()),
            "claude-opus-5-5".into(),
            Effort::Medium,
            fast_retry(),
        )
        .unwrap();
        let err = provider.stream(&request(), &|_| {}, &CancellationToken::new()).await.unwrap_err();
        assert!(matches!(&err, ProviderError::InvalidRequest(m) if m.contains("<redacted>") && !m.contains(KEY)));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_aborts_a_pending_request() {
        // A server that accepts but never answers.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let provider = AnthropicProvider::with_base_url(
            &url,
            Secret::new(KEY.to_owned()),
            "claude-opus-5-5".into(),
            Effort::Medium,
            fast_retry(),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let started = std::time::Instant::now();
        let err = provider.stream(&request(), &|_| {}, &cancel).await.unwrap_err();
        assert_eq!(err, ProviderError::Canceled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
