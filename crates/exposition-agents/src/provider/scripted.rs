//! Scripted provider — development only.
//!
//! Compiled only into debug builds (`cfg(debug_assertions)`), so it can never
//! be selected in a release build. It implements the same [`Provider`] trait as
//! the Anthropic adapter and produces the same content-block shapes, so the
//! task loop, tools, persistence, review, and UI are exercised for real. It
//! cannot establish live model behavior.
//!
//! A script is a list of turns, used in order: the n-th model request of a
//! task (across runs, including canceled ones) gets turn n. Strings of the form
//! `"$ref:<tool_use_id>/<json-pointer>"` inside tool inputs are replaced with
//! the value at that pointer in the earlier tool result (parsed as JSON), so a
//! script can use the revision returned by `read_file`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{EventSink, Provider, ProviderError, ProviderRequest, ProviderResponse, StreamEvent, Usage};
use crate::model::ProviderKind;

pub const LABEL: &str = "Scripted provider — development only";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Script {
    pub turns: Vec<ScriptTurn>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScriptTurn {
    pub steps: Vec<ScriptStep>,
    /// Overrides the inferred stop reason (`tool_use` if any tool is used,
    /// otherwise `end_turn`).
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Delay between streamed text chunks.
    #[serde(default)]
    pub chunk_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ScriptStep {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool call whose streamed input is not valid JSON.
    RawToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
    DelayMs(u64),
    /// Block until the request is canceled.
    WaitForCancel(bool),
    Refusal {
        category: Option<String>,
        explanation: Option<String>,
    },
    Fallback {
        from: String,
        to: String,
    },
    /// Fail this attempt with an error (the task loop sees provider errors).
    Error {
        kind: String,
        message: String,
    },
}

pub struct ScriptedProvider {
    script: Script,
    /// Requests served for the task so far (shared across runs).
    requests: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    pub fn new(script: Script, requests: Arc<AtomicUsize>) -> Self {
        Self { script, requests }
    }
}

fn resolve_refs(value: &Value, request: &ProviderRequest) -> Value {
    match value {
        Value::String(s) => match s.strip_prefix("$ref:") {
            Some(reference) => lookup_ref(reference, request).unwrap_or(Value::Null),
            None => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve_refs(v, request)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), resolve_refs(v, request))).collect()),
        other => other.clone(),
    }
}

fn lookup_ref(reference: &str, request: &ProviderRequest) -> Option<Value> {
    let (id, pointer) =
        reference.split_once('/').map(|(a, b)| (a, format!("/{b}"))).unwrap_or((reference, String::new()));
    for message in request.messages.iter().rev() {
        for block in &message.content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result")
                && block.get("tool_use_id").and_then(Value::as_str) == Some(id)
            {
                let text = block.get("content").and_then(Value::as_str)?;
                let parsed: Value = serde_json::from_str(text).ok()?;
                return parsed.pointer(&pointer).cloned();
            }
        }
    }
    None
}

async fn pause(ms: u64, cancel: &CancellationToken) -> Result<(), ProviderError> {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
        _ = cancel.cancelled() => Err(ProviderError::Canceled),
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Scripted
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        sink: EventSink<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderResponse, ProviderError> {
        let index = self.requests.fetch_add(1, Ordering::SeqCst);
        let turn =
            self.script.turns.get(index).ok_or_else(|| {
                ProviderError::InvalidRequest(format!("{LABEL}: the script has no turn {}", index + 1))
            })?;
        let model = "scripted-development-model".to_owned();
        sink(StreamEvent::MessageStart { model: model.clone() });
        let chunk_delay = turn.chunk_delay_ms.unwrap_or(10);
        let mut content = Vec::new();
        let mut invalid = std::collections::HashMap::new();
        let mut stop_reason = None;
        let mut stop_details = None;
        let mut used_tools = false;
        for step in &turn.steps {
            match step {
                ScriptStep::Text(text) => {
                    for (i, word) in text.split_inclusive(' ').enumerate() {
                        if i > 0 && chunk_delay > 0 {
                            pause(chunk_delay, cancel).await?;
                        }
                        sink(StreamEvent::TextDelta(word.to_owned()));
                    }
                    content.push(json!({"type": "text", "text": text}));
                }
                ScriptStep::Thinking(text) => {
                    sink(StreamEvent::ThinkingDelta(text.clone()));
                    content.push(json!({"type": "thinking", "thinking": text, "signature": "scripted-signature"}));
                }
                ScriptStep::ToolUse { id, name, input } => {
                    used_tools = true;
                    sink(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
                    content.push(
                        json!({"type": "tool_use", "id": id, "name": name, "input": resolve_refs(input, request)}),
                    );
                }
                ScriptStep::RawToolUse { id, name, partial_json } => {
                    used_tools = true;
                    sink(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
                    content.push(json!({"type": "tool_use", "id": id, "name": name, "input": {}}));
                    invalid.insert(id.clone(), partial_json.clone());
                }
                ScriptStep::DelayMs(ms) => pause(*ms, cancel).await?,
                ScriptStep::WaitForCancel(_) => {
                    cancel.cancelled().await;
                    return Err(ProviderError::Canceled);
                }
                ScriptStep::Refusal { category, explanation } => {
                    stop_reason = Some("refusal".to_owned());
                    stop_details = Some(json!({"type": "refusal", "category": category, "explanation": explanation}));
                }
                ScriptStep::Fallback { from, to } => {
                    sink(StreamEvent::Fallback { from: from.clone(), to: to.clone() });
                    content.push(json!({"type": "fallback", "from": {"model": from}, "to": {"model": to}}));
                }
                ScriptStep::Error { kind, message } => {
                    return Err(super::sse::stream_error(&json!({"error": {"type": kind, "message": message}})));
                }
            }
        }
        let mut stop_reason = stop_reason
            .or_else(|| turn.stop_reason.clone())
            .unwrap_or_else(|| if used_tools { "tool_use".into() } else { "end_turn".into() });
        let input_chars: usize =
            request.messages.iter().map(|m| Value::Array(m.content.clone()).to_string().len()).sum();
        let output_chars = Value::Array(content.clone()).to_string().len();
        let mut output_tokens = (output_chars / 4) as u64 + 1;
        // Like the real API: output beyond max_tokens is cut off.
        if output_tokens > u64::from(request.max_tokens) && stop_reason != "refusal" {
            output_tokens = u64::from(request.max_tokens);
            stop_reason = "max_tokens".into();
        }
        Ok(ProviderResponse {
            model,
            content,
            stop_reason,
            stop_details,
            usage: Usage {
                input_tokens: (input_chars / 4 + request.system.len() / 4) as u64,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            invalid_tool_inputs: invalid,
        })
    }
}
