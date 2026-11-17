//! OpenAI Responses API adapter (API-key connection; raw HTTPS like the
//! Anthropic adapter, no SDK).
//!
//! Request (developers.openai.com, checked 2026-11-17): `POST /v1/responses`
//! with `Authorization: Bearer <key>` (+ optional `OpenAI-Organization` /
//! `OpenAI-Project`), `stream: true`, `store: false` (the conversation stays
//! in Exposition; every request carries the full history), function tools
//! (`strict` only for schemas that meet the strict-mode rules), `tool_choice:
//! auto`, and for reasoning models `reasoning.effort` plus
//! `include: ["reasoning.encrypted_content"]` so reasoning items can be
//! replayed statelessly with their tool calls.
//!
//! History conversion: Exposition's canonical history is Anthropic
//! content-block JSON. Text, images (`input_image` data URLs), PDFs
//! (`input_file`), `tool_use` (→ `function_call`) and `tool_result`
//! (→ `function_call_output`) map both ways. Opaque provider state stays with
//! its adapter: OpenAI reasoning items are stored as `openai_reasoning` blocks
//! and sent back only to OpenAI; Anthropic `thinking`/`redacted_thinking`/
//! `fallback` blocks are never forwarded here.
//!
//! Streaming: text and reasoning-summary deltas are forwarded; the final
//! items come from `response.output_item.done` / `response.completed`. A
//! stream that ends without `response.completed`, `response.incomplete`, or a
//! failure is an error, never success. Retries follow the Anthropic adapter
//! (429 except exhausted quota, 5xx, connection failures), with a `Reset`
//! event when output had already streamed; tool calls run only after a
//! complete response, so a retry never repeats a side effect.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::anthropic::{RetryPolicy, describe_reqwest};
use super::sse::{SseDecoder, SseEvent, bounded};
use super::{EventSink, Provider, ProviderError, ProviderRequest, ProviderResponse, StreamEvent, Usage, block_type};
use crate::credentials::Secret;
use crate::model::{Effort, OpenAiConnection, ProviderKind};

pub const API_BASE: &str = "https://api.openai.com";
/// Per-request output ceiling for streamed agent turns.
pub const MAX_OUTPUT_TOKENS: u32 = 64_000;
/// Opaque reasoning items kept in the canonical history (OpenAI only).
pub const REASONING_BLOCK: &str = "openai_reasoning";

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Secret,
    model: String,
    /// `None`: the model takes no reasoning effort (no reasoning items requested).
    effort: Option<Effort>,
    connection: OpenAiConnection,
    retry: RetryPolicy,
    /// The model's output limit when it is below the agent's per-turn ceiling.
    output_cap: Option<u64>,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .finish_non_exhaustive()
    }
}

/// Whether a JSON schema satisfies the strict-mode rules (every object closed
/// with `additionalProperties: false` and every property required).
pub fn strict_compatible(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => {
            let is_object = map.get("type").and_then(Value::as_str) == Some("object") || map.contains_key("properties");
            if is_object {
                if map.get("additionalProperties") != Some(&Value::Bool(false)) {
                    return false;
                }
                let props = map.get("properties").and_then(Value::as_object);
                let required: Vec<&str> = map
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if let Some(props) = props
                    && props.keys().any(|k| !required.contains(&k.as_str()))
                {
                    return false;
                }
            }
            map.iter().all(|(k, v)| k == "enum" || k == "const" || k == "default" || strict_compatible(v))
        }
        Value::Array(items) => items.iter().all(strict_compatible),
        _ => true,
    }
}

impl OpenAiProvider {
    pub fn with_base_url(
        base_url: &str,
        api_key: Secret,
        model: String,
        effort: Option<Effort>,
        connection: OpenAiConnection,
        retry: RetryPolicy,
    ) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(300))
            .user_agent(concat!("Exposition/", env!("CARGO_PKG_VERSION")))
            // Never follow a redirect: it would re-send the key and the body.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Network(format!("cannot create HTTP client: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model,
            effort,
            connection,
            retry,
            output_cap: None,
        })
    }

    /// Cap `max_output_tokens` at the model's own limit.
    pub fn with_output_cap(mut self, cap: Option<u64>) -> Self {
        self.output_cap = cap;
        self
    }

    /// The JSON body for one streamed request.
    pub fn request_body(&self, request: &ProviderRequest) -> Value {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": tool.strict && strict_compatible(&tool.input_schema),
                })
            })
            .collect();
        let mut input =
            to_input(&request.messages.iter().map(|m| (m.role.as_str(), m.content.as_slice())).collect::<Vec<_>>());
        if self.effort.is_none() {
            // A model without reasoning rejects reasoning input items (e.g.
            // after switching from a reasoning model in the same task).
            input.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
        }
        let ceiling = u64::from(request.max_tokens.min(MAX_OUTPUT_TOKENS));
        let max_output = self.output_cap.map_or(ceiling, |cap| ceiling.min(cap));
        let mut body = json!({
            "model": self.model,
            "instructions": request.system,
            "input": input,
            "tools": tools,
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "store": false,
            "stream": true,
            "max_output_tokens": max_output,
        });
        if let Some(effort) = self.effort {
            body["reasoning"] = json!({"effort": effort.as_str(), "summary": "auto"});
            body["include"] = json!(["reasoning.encrypted_content"]);
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
        let jitter_ms = (u64::from(std::process::id()) * 7919 + u64::from(attempt) * 104_729)
            % (self.retry.base_delay.as_millis() as u64 / 2 + 1);
        (exp + Duration::from_millis(jitter_ms)).min(self.retry.max_delay)
    }

    fn http_error(&self, status: u16, body: &str, retry_after_ms: Option<u64>) -> ProviderError {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let message = parsed
            .as_ref()
            .and_then(|v| v.pointer("/error/message").and_then(Value::as_str))
            .map(|m| self.redact(m))
            .unwrap_or_else(|| format!("HTTP {status}"));
        let code = parsed.as_ref().and_then(|v| v.pointer("/error/code").and_then(Value::as_str)).unwrap_or("");
        match status {
            401 => ProviderError::Authentication(message),
            403 => ProviderError::Authentication(format!("permission denied: {message}")),
            // Exhausted quota or billing: retrying cannot help.
            429 if matches!(code, "insufficient_quota" | "billing_hard_limit_reached") => {
                ProviderError::InvalidRequest(format!(
                    "OpenAI reports the account's usage limit is reached ({code}): {message}"
                ))
            }
            429 => ProviderError::RateLimited { retry_after_ms, message },
            503 => ProviderError::Overloaded(message),
            300..=399 => ProviderError::InvalidRequest(format!(
                "the provider answered with a redirect (HTTP {status}); Exposition does not follow redirects, so \
                 nothing was sent to another address"
            )),
            400 | 402 | 404 | 409 | 413 | 422 => ProviderError::InvalidRequest(message),
            500..=599 => ProviderError::Server { status, message },
            _ => ProviderError::InvalidRequest(format!("unexpected HTTP {status}: {message}")),
        }
    }

    async fn attempt(
        &self,
        body: &[u8],
        sink: EventSink<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderResponse, (ProviderError, bool)> {
        let mut builder = self
            .client
            .post(format!("{}/v1/responses", self.base_url))
            .bearer_auth(self.api_key.expose())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if let Some(org) = self.connection.organization.as_deref().filter(|s| !s.is_empty()) {
            builder = builder.header("OpenAI-Organization", org);
        }
        if let Some(project) = self.connection.project.as_deref().filter(|s| !s.is_empty()) {
            builder = builder.header("OpenAI-Project", project);
        }
        let response = tokio::select! {
            r = builder.body(body.to_vec()).send() => r.map_err(|e| (ProviderError::Network(self.redact(&describe_reqwest(&e))), false))?,
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
        let mut assembler = ResponseAssembler::default();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                _ = cancel.cancelled() => return Err((ProviderError::Canceled, assembler.emitted)),
            };
            match next {
                Some(Ok(chunk)) => {
                    for event in decoder.push(&chunk) {
                        let emitted = assembler.emitted;
                        assembler.handle(&event, sink).map_err(|e| (self.redact_error(e), emitted))?;
                    }
                    if assembler.done {
                        break;
                    }
                }
                Some(Err(e)) => {
                    return Err((ProviderError::Network(self.redact(&describe_reqwest(&e))), assembler.emitted));
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
            ProviderError::Protocol(m) => ProviderError::Protocol(self.redact(&m)),
            other => other,
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
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
                    tracing::warn!(attempt, delay_ms = delay.as_millis() as u64, "retrying OpenAI request: {error}");
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

// ---------------------------------------------------------------- history → input

fn tool_result_text(block: &Value) -> String {
    let text = match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    };
    if block.get("is_error").and_then(Value::as_bool) == Some(true) { format!("Error: {text}") } else { text }
}

fn data_url(source: &Value) -> Option<String> {
    let media = source.get("media_type").and_then(Value::as_str)?;
    let data = source.get("data").and_then(Value::as_str)?;
    Some(format!("data:{media};base64,{data}"))
}

/// Convert canonical (Anthropic content-block) messages to Responses input items.
pub fn to_input(messages: &[(&str, &[Value])]) -> Vec<Value> {
    let mut items = Vec::new();
    for (role, content) in messages {
        if *role == "assistant" {
            let mut text = String::new();
            let flush = |text: &mut String, items: &mut Vec<Value>| {
                if !text.is_empty() {
                    items.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": std::mem::take(text)}]}));
                }
            };
            for block in content.iter() {
                match block_type(block) {
                    "text" => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
                    "tool_use" => {
                        flush(&mut text, &mut items);
                        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        items.push(json!({
                            "type": "function_call",
                            "call_id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                            "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                            "arguments": input.to_string(),
                        }));
                    }
                    t if t == REASONING_BLOCK => {
                        flush(&mut text, &mut items);
                        let mut item = json!({"type": "reasoning", "summary": block.get("summary").cloned().unwrap_or_else(|| json!([]))});
                        for key in ["id", "encrypted_content"] {
                            if let Some(v) = block.get(key) {
                                item[key] = v.clone();
                            }
                        }
                        items.push(item);
                    }
                    // Another provider's private state or notices: never forwarded.
                    _ => {}
                }
            }
            flush(&mut text, &mut items);
            continue;
        }
        // User turn: tool results first (as their own items), then the message.
        let mut parts = Vec::new();
        for block in content.iter() {
            match block_type(block) {
                "tool_result" => items.push(json!({
                    "type": "function_call_output",
                    "call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                    "output": tool_result_text(block),
                })),
                "text" => parts.push(
                    json!({"type": "input_text", "text": block.get("text").and_then(Value::as_str).unwrap_or("")}),
                ),
                "image" => {
                    if let Some(url) = block.get("source").and_then(data_url) {
                        parts.push(json!({"type": "input_image", "image_url": url, "detail": "auto"}));
                    }
                }
                "document" => {
                    if let Some(url) = block.get("source").and_then(data_url) {
                        let name = block.get("title").and_then(Value::as_str).unwrap_or("document.pdf");
                        parts.push(json!({"type": "input_file", "filename": name, "file_data": url}));
                    }
                }
                _ => {}
            }
        }
        if !parts.is_empty() {
            items.push(json!({"type": "message", "role": "user", "content": parts}));
        }
    }
    items
}

// ---------------------------------------------------------------- stream assembly

#[derive(Debug, Default)]
struct ResponseAssembler {
    model: String,
    /// Final items by output index.
    items: BTreeMap<u64, Value>,
    /// Streamed text by output index (used when a final item is missing).
    text: HashMap<u64, String>,
    status: Option<String>,
    incomplete_reason: Option<String>,
    usage: Usage,
    done: bool,
    emitted: bool,
}

fn protocol(message: impl Into<String>) -> ProviderError {
    ProviderError::Protocol(message.into())
}

impl ResponseAssembler {
    fn handle(&mut self, event: &SseEvent, sink: EventSink<'_>) -> Result<(), ProviderError> {
        if event.data == "[DONE]" {
            return Ok(());
        }
        let data: Value =
            serde_json::from_str(&event.data).map_err(|e| protocol(format!("event data is not JSON: {e}")))?;
        let kind = data.get("type").and_then(Value::as_str).or(event.event.as_deref()).unwrap_or("");
        let index = data.get("output_index").and_then(Value::as_u64).unwrap_or(0);
        match kind {
            "response.created" | "response.in_progress" => {
                if let Some(model) = data.pointer("/response/model").and_then(Value::as_str)
                    && self.model.is_empty()
                {
                    self.model = model.to_owned();
                    sink(StreamEvent::MessageStart { model: model.to_owned() });
                }
            }
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    self.emitted = true;
                    sink(StreamEvent::ToolUseStart {
                        id: item.get("call_id").and_then(Value::as_str).unwrap_or("").to_owned(),
                        name: item.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
                    });
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = data.get("delta").and_then(Value::as_str).unwrap_or("");
                self.text.entry(index).or_default().push_str(delta);
                if kind == "response.output_text.delta" && !delta.is_empty() {
                    self.emitted = true;
                    sink(StreamEvent::TextDelta(delta.to_owned()));
                }
            }
            "response.reasoning_summary_text.delta" => {
                let delta = data.get("delta").and_then(Value::as_str).unwrap_or("");
                if !delta.is_empty() {
                    self.emitted = true;
                    sink(StreamEvent::ThinkingDelta(delta.to_owned()));
                }
            }
            "response.output_item.done" => {
                if let Some(item) = data.get("item") {
                    self.items.insert(index, item.clone());
                }
            }
            "response.completed" | "response.incomplete" => {
                let response = data.get("response").cloned().unwrap_or(Value::Null);
                if let Some(output) = response.get("output").and_then(Value::as_array)
                    && !output.is_empty()
                {
                    self.items = output.iter().enumerate().map(|(i, item)| (i as u64, item.clone())).collect();
                }
                if let Some(model) = response.get("model").and_then(Value::as_str) {
                    self.model = model.to_owned();
                }
                self.status = response.get("status").and_then(Value::as_str).map(str::to_owned);
                self.incomplete_reason =
                    response.pointer("/incomplete_details/reason").and_then(Value::as_str).map(str::to_owned);
                if let Some(usage) = response.get("usage") {
                    let cached =
                        usage.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    self.usage = Usage {
                        input_tokens: input.saturating_sub(cached),
                        output_tokens: usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
                        cache_read_tokens: cached,
                        cache_write_tokens: 0,
                    };
                }
                self.done = true;
            }
            "response.failed" => {
                let error = data.pointer("/response/error").cloned().unwrap_or(Value::Null);
                return Err(stream_error(&error));
            }
            "error" => return Err(stream_error(data.get("error").unwrap_or(&data))),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<ProviderResponse, ProviderError> {
        if !self.done {
            return Err(ProviderError::Network("the response stream ended before the response completed".into()));
        }
        let mut content = Vec::new();
        let mut invalid_tool_inputs = HashMap::new();
        let mut refusal = None;
        let mut tool_use = false;
        for (index, item) in &self.items {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    let mut text = String::new();
                    for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                text.push_str(part.get("text").and_then(Value::as_str).unwrap_or(""))
                            }
                            Some("refusal") => refusal = part.get("refusal").and_then(Value::as_str).map(str::to_owned),
                            _ => {}
                        }
                    }
                    if text.is_empty() {
                        text = self.text.get(index).cloned().unwrap_or_default();
                    }
                    if !text.is_empty() {
                        content.push(json!({"type": "text", "text": text}));
                    }
                }
                "function_call" => {
                    tool_use = true;
                    let id = item.get("call_id").and_then(Value::as_str).unwrap_or("").to_owned();
                    let raw = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                    let input = match serde_json::from_str::<Value>(if raw.trim().is_empty() { "{}" } else { raw }) {
                        Ok(v) if v.is_object() => v,
                        _ => {
                            invalid_tool_inputs.insert(id.clone(), raw.to_owned());
                            json!({})
                        }
                    };
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
                }
                "reasoning" => {
                    let summary: Vec<Value> =
                        item.get("summary").and_then(Value::as_array).cloned().unwrap_or_default();
                    let mut block = json!({"type": REASONING_BLOCK, "summary": summary});
                    for key in ["id", "encrypted_content"] {
                        if let Some(v) = item.get(key) {
                            block[key] = v.clone();
                        }
                    }
                    content.push(block);
                }
                _ => {}
            }
        }
        let (stop_reason, stop_details) = if let Some(text) = refusal {
            content.push(json!({"type": "text", "text": text}));
            ("refusal".to_owned(), None)
        } else if self.status.as_deref() == Some("incomplete") {
            let reason = self.incomplete_reason.clone().unwrap_or_else(|| "unknown".into());
            if reason == "content_filter" {
                ("refusal".to_owned(), Some(json!({"reason": reason})))
            } else {
                ("max_tokens".to_owned(), Some(json!({"reason": reason})))
            }
        } else if tool_use {
            ("tool_use".to_owned(), None)
        } else {
            ("end_turn".to_owned(), None)
        };
        Ok(ProviderResponse {
            model: self.model,
            content,
            stop_reason,
            stop_details,
            usage: self.usage,
            invalid_tool_inputs,
        })
    }
}

fn stream_error(error: &Value) -> ProviderError {
    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
    let message = bounded(error.get("message").and_then(Value::as_str).unwrap_or("the response failed"));
    match code {
        "rate_limit_exceeded" => ProviderError::RateLimited { retry_after_ms: None, message },
        "server_error" | "internal_error" => ProviderError::Server { status: 500, message },
        "server_is_overloaded" | "overloaded" => ProviderError::Overloaded(message),
        "insufficient_quota" => {
            ProviderError::InvalidRequest(format!("OpenAI reports the account's usage limit is reached: {message}"))
        }
        _ => ProviderError::InvalidRequest(if code.is_empty() { message } else { format!("{message} ({code})") }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::provider::{ProviderMessage, ToolDefinition};

    const KEY: &str = "sk-proj-test-0123456789abcdef";

    fn provider(base: &str, effort: Option<Effort>) -> OpenAiProvider {
        OpenAiProvider::with_base_url(
            base,
            Secret::new(KEY.to_owned()),
            "gpt-6.2-sol".into(),
            effort,
            OpenAiConnection { organization: Some("org-1".into()), project: Some("proj_1".into()) },
            RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(5), max_delay: Duration::from_millis(20) },
        )
        .unwrap()
    }

    fn request(messages: Vec<ProviderMessage>) -> ProviderRequest {
        ProviderRequest {
            system: "system prompt".into(),
            tools: vec![
                ToolDefinition {
                    name: "read_file",
                    description: "Read a file",
                    input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false}),
                    strict: true,
                },
                ToolDefinition {
                    name: "loose",
                    description: "Optional fields",
                    input_schema: json!({"type": "object", "properties": {"a": {"type": "string"}}, "additionalProperties": false}),
                    strict: true,
                },
            ],
            messages,
            max_tokens: 100_000,
        }
    }

    fn user(text: &str) -> ProviderMessage {
        ProviderMessage { role: "user".into(), content: vec![json!({"type": "text", "text": text})] }
    }

    #[test]
    fn request_body_follows_the_responses_api() {
        let p = provider(API_BASE, Some(Effort::High));
        let body = p.request_body(&request(vec![user("hi")]));
        assert_eq!(body["model"], "gpt-6.2-sol");
        assert_eq!(body["instructions"], "system prompt");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_output_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(body["reasoning"], json!({"effort": "high", "summary": "auto"}));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["parameters"]["required"], json!(["path"]));
        assert_eq!(body["tools"][0]["strict"], true);
        // A schema with an optional property cannot be strict.
        assert_eq!(body["tools"][1]["strict"], false);
        assert_eq!(
            body["input"],
            json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}])
        );
        assert!(!body.to_string().contains(KEY) && !format!("{p:?}").contains(KEY));
        let plain = provider(API_BASE, None).request_body(&request(vec![user("hi")]));
        assert!(plain.get("reasoning").is_none() && plain.get("include").is_none());
        // A model without reasoning gets no reasoning items, and its own output limit.
        let history = vec![
            user("hi"),
            ProviderMessage {
                role: "assistant".into(),
                content: vec![
                    json!({"type": REASONING_BLOCK, "id": "rs_1", "encrypted_content": "enc", "summary": []}),
                    json!({"type": "text", "text": "Hello."}),
                ],
            },
            user("again"),
        ];
        let capped = provider(API_BASE, None).with_output_cap(Some(16_384)).request_body(&request(history.clone()));
        assert!(capped["input"].as_array().unwrap().iter().all(|i| i["type"] != "reasoning"));
        assert_eq!(capped["max_output_tokens"], 16_384);
        let reasoning = provider(API_BASE, Some(Effort::Low)).request_body(&request(history));
        assert!(reasoning["input"].as_array().unwrap().iter().any(|i| i["type"] == "reasoning"));
    }

    #[test]
    fn history_converts_both_ways_and_drops_other_providers_private_state() {
        let history = [
            (
                "user",
                vec![
                    json!({"type": "text", "text": "look"}),
                    json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}),
                    json!({"type": "document", "title": "paper.pdf", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBE"}}),
                ],
            ),
            (
                "assistant",
                vec![
                    json!({"type": "thinking", "thinking": "private", "signature": "sig"}),
                    json!({"type": REASONING_BLOCK, "id": "rs_1", "encrypted_content": "enc", "summary": []}),
                    json!({"type": "text", "text": "Reading."}),
                    json!({"type": "tool_use", "id": "call_1", "name": "read_file", "input": {"path": "a.md"}}),
                    json!({"type": "fallback", "from": {"model": "x"}, "to": {"model": "y"}}),
                ],
            ),
            (
                "user",
                vec![
                    json!({"type": "tool_result", "tool_use_id": "call_1", "content": "# A", "is_error": false}),
                    json!({"type": "text", "text": "and?"}),
                ],
            ),
        ];
        let refs: Vec<(&str, &[Value])> = history.iter().map(|(r, c)| (*r, c.as_slice())).collect();
        let input = to_input(&refs);
        let types: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["message", "reasoning", "message", "function_call", "function_call_output", "message"]);
        assert_eq!(
            input[0]["content"][1],
            json!({"type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "auto"})
        );
        assert_eq!(input[0]["content"][2]["file_data"], "data:application/pdf;base64,JVBE");
        assert_eq!(input[0]["content"][2]["filename"], "paper.pdf");
        assert_eq!(input[1]["encrypted_content"], "enc");
        assert_eq!(input[3]["arguments"], "{\"path\":\"a.md\"}");
        assert_eq!(input[4], json!({"type": "function_call_output", "call_id": "call_1", "output": "# A"}));
        assert!(!serde_json::to_string(&input).unwrap().contains("private"));
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e)).collect()
    }

    fn completed(output: Value, status: &str, extra: Value) -> Value {
        let mut response = json!({"id": "resp_1", "model": "gpt-6.2-sol-2026-10-01", "status": status, "output": output,
            "usage": {"input_tokens": 1200, "input_tokens_details": {"cached_tokens": 1000}, "output_tokens": 40}});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                response[k] = v.clone();
            }
        }
        json!({"type": if status == "completed" { "response.completed" } else { "response.incomplete" }, "response": response})
    }

    /// Serves raw HTTP responses in order; records request bodies and headers.
    async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(head) = text.find("\r\n\r\n") {
                        let len = text[..head]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= head + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                seen2.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn ok_stream(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
    }

    fn collect() -> (Arc<Mutex<Vec<StreamEvent>>>, impl Fn(StreamEvent) + Send + Sync) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = Arc::clone(&events);
        (events, move |e| e2.lock().unwrap().push(e))
    }

    #[tokio::test]
    async fn streams_text_reasoning_and_a_tool_call() {
        let fc = json!({"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read_file", "arguments": "{\"path\":\"main.typ\"}"});
        let reasoning = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAA", "summary": [{"type": "summary_text", "text": "Plan"}]});
        let message = json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Let me look."}]});
        let body = sse(&[
            json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-6.2-sol-2026-10-01", "status": "in_progress"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "Plan"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "Let me "}),
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "look."}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": message}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "call_id": "call_9", "name": "read_file", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"path\""}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": fc}),
            completed(json!([reasoning, message, fc]), "completed", json!({})),
        ]);
        let (base, seen) = serve(vec![ok_stream(&body)]).await;
        let (events, sink) = collect();
        let r = provider(&base, Some(Effort::Medium))
            .stream(&request(vec![user("hi")]), &sink, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.stop_reason, "tool_use");
        assert_eq!(r.model, "gpt-6.2-sol-2026-10-01");
        assert_eq!(r.content[0]["type"], REASONING_BLOCK);
        assert_eq!(r.content[1], json!({"type": "text", "text": "Let me look."}));
        assert_eq!(
            r.content[2],
            json!({"type": "tool_use", "id": "call_9", "name": "read_file", "input": {"path": "main.typ"}})
        );
        assert_eq!(
            r.usage,
            Usage { input_tokens: 200, output_tokens: 40, cache_read_tokens: 1000, cache_write_tokens: 0 }
        );
        let events = events.lock().unwrap().clone();
        assert!(events.contains(&StreamEvent::ThinkingDelta("Plan".into())));
        assert!(events.contains(&StreamEvent::ToolUseStart { id: "call_9".into(), name: "read_file".into() }));
        let raw = seen.lock().unwrap()[0].clone();
        assert!(raw.starts_with("POST /v1/responses"));
        let lower = raw.to_ascii_lowercase();
        assert!(lower.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())));
        assert!(lower.contains("openai-organization: org-1") && lower.contains("openai-project: proj_1"));
        // The continuation replays the reasoning item with its tool call.
        let next = to_input(&[("assistant", r.content.as_slice())]);
        assert_eq!(next[0]["type"], "reasoning");
        assert_eq!(next[0]["encrypted_content"], "gAAA");
    }

    #[tokio::test]
    async fn invalid_tool_arguments_refusals_and_incomplete_responses() {
        let bad = json!({"type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":"});
        let body = sse(&[completed(json!([bad]), "completed", json!({}))]);
        let (base, _) = serve(vec![ok_stream(&body)]).await;
        let (_, sink) = collect();
        let r =
            provider(&base, None).stream(&request(vec![user("hi")]), &sink, &CancellationToken::new()).await.unwrap();
        assert_eq!(r.invalid_tool_inputs.get("call_1").map(String::as_str), Some("{\"path\":"));

        let refusal =
            json!({"type": "message", "content": [{"type": "refusal", "refusal": "I can't help with that."}]});
        let body = sse(&[completed(json!([refusal]), "completed", json!({}))]);
        let (base, _) = serve(vec![ok_stream(&body)]).await;
        let r =
            provider(&base, None).stream(&request(vec![user("hi")]), &sink, &CancellationToken::new()).await.unwrap();
        assert_eq!(r.stop_reason, "refusal");

        let partial = json!({"type": "message", "content": [{"type": "output_text", "text": "Partial"}]});
        let body = sse(&[completed(
            json!([partial]),
            "incomplete",
            json!({"incomplete_details": {"reason": "max_output_tokens"}}),
        )]);
        let (base, _) = serve(vec![ok_stream(&body)]).await;
        let r =
            provider(&base, None).stream(&request(vec![user("hi")]), &sink, &CancellationToken::new()).await.unwrap();
        assert_eq!(r.stop_reason, "max_tokens");
    }

    #[tokio::test]
    async fn a_disconnected_stream_is_an_error_and_is_retried_with_a_reset() {
        let cut = sse(&[json!({"type": "response.output_text.delta", "output_index": 0, "delta": "Half"})]);
        let message = json!({"type": "message", "content": [{"type": "output_text", "text": "Whole"}]});
        let whole = sse(&[completed(json!([message]), "completed", json!({}))]);
        let (base, seen) = serve(vec![ok_stream(&cut), ok_stream(&whole)]).await;
        let (events, sink) = collect();
        let r =
            provider(&base, None).stream(&request(vec![user("hi")]), &sink, &CancellationToken::new()).await.unwrap();
        assert_eq!(r.content, vec![json!({"type": "text", "text": "Whole"})]);
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, StreamEvent::Reset { .. })));
    }

    #[tokio::test]
    async fn errors_are_classified_and_never_contain_the_key() {
        let reply = |status: &str, body: Value| {
            format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.to_string().len()
            )
        };
        let (_, sink) = collect();
        let (base, seen) = serve(vec![reply(
            "401 Unauthorized",
            json!({"error": {"message": format!("Incorrect API key provided: {KEY}"), "code": "invalid_api_key"}}),
        )])
        .await;
        let e = provider(&base, None)
            .stream(&request(vec![user("hi")]), &sink, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, ProviderError::Authentication(m) if !m.contains(KEY) && m.contains("<redacted>")),
            "{e:?}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Exhausted quota is not retried.
        let (base, seen) = serve(vec![reply(
            "429 Too Many Requests",
            json!({"error": {"message": "quota", "code": "insufficient_quota"}}),
        )])
        .await;
        let e = provider(&base, None)
            .stream(&request(vec![user("hi")]), &sink, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(&e, ProviderError::InvalidRequest(m) if m.contains("usage limit")), "{e:?}");
        assert_eq!(seen.lock().unwrap().len(), 1);
        // A failed response in the stream.
        let body = sse(&[
            json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "invalid_prompt", "message": "bad"}}}),
        ]);
        let (base, _) = serve(vec![ok_stream(&body)]).await;
        let e = provider(&base, None)
            .stream(&request(vec![user("hi")]), &sink, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn cancellation_aborts_the_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c2.cancel();
        });
        let (_, sink) = collect();
        let started = std::time::Instant::now();
        let e = provider(&format!("http://{addr}"), None)
            .stream(&request(vec![user("hi")]), &sink, &cancel)
            .await
            .unwrap_err();
        assert_eq!(e, ProviderError::Canceled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn strict_compatibility() {
        assert!(strict_compatible(
            &json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false})
        ));
        assert!(!strict_compatible(&json!({"type": "object", "properties": {"a": {"type": "string"}}})));
        assert!(!strict_compatible(
            &json!({"type": "object", "properties": {"a": {"type": "object", "properties": {"b": {"type": "string"}}, "required": ["b"]}}, "required": ["a"], "additionalProperties": false})
        ));
    }
}
