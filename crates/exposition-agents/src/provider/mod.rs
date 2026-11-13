//! Model providers.
//!
//! The canonical message format is the Anthropic Messages API content-block
//! JSON: assistant content is stored and replayed exactly as received
//! (thinking blocks with their signatures, fallback markers, tool_use blocks),
//! keeping the history append-only.

pub mod anthropic;
#[cfg(debug_assertions)]
pub mod scripted;
pub mod sse;

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::ProviderKind;

/// A client tool offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    /// Ask the provider to constrain generation to the schema (`strict`).
    /// The provider compiles every strict schema of a request into one
    /// grammar with a size limit, so large schemas are sent non-strict; every
    /// tool's input is deserialized and validated in Rust either way.
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderMessage {
    pub role: String,
    pub content: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequest {
    /// Stable system prompt (cached).
    pub system: String,
    /// Stable, deterministic tool list (cached).
    pub tools: Vec<ToolDefinition>,
    pub messages: Vec<ProviderMessage>,
    pub max_tokens: u32,
}

/// Progress while a response streams.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        model: String,
    },
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    /// A server-side fallback switched models.
    Fallback {
        from: String,
        to: String,
    },
    /// A retry discarded partially streamed output.
    Reset {
        reason: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderResponse {
    /// Model that produced the message (the fallback model after a fallback).
    pub model: String,
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub stop_details: Option<Value>,
    pub usage: Usage,
    /// Tool-use blocks whose streamed input was not valid JSON, by id (raw text).
    pub invalid_tool_inputs: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("the request was canceled")]
    Canceled,
    #[error("the model provider is not configured: {0}")]
    NotConfigured(String),
    #[error("the provider rejected the API key ({0})")]
    Authentication(String),
    #[error("the provider rejected the request: {0}")]
    InvalidRequest(String),
    #[error("the provider rate-limited the request: {message}")]
    RateLimited { retry_after_ms: Option<u64>, message: String },
    #[error("the provider is overloaded: {0}")]
    Overloaded(String),
    #[error("provider error (HTTP {status}): {message}")]
    Server { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("malformed response stream: {0}")]
    Protocol(String),
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Overloaded(_)
                | ProviderError::Server { .. }
                | ProviderError::Network(_)
        )
    }
}

pub type EventSink<'a> = &'a (dyn Fn(StreamEvent) + Send + Sync);

#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    /// Stream one model response. Cancellation aborts the request/stream.
    async fn stream(
        &self,
        request: &ProviderRequest,
        sink: EventSink<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderResponse, ProviderError>;
}

/// Prepare received assistant content for the history and for execution.
///
/// After a mid-output server-side fallback, `thinking`, `redacted_thinking`,
/// `tool_use`, and unpaired `server_tool_use` blocks that appear before the
/// final `fallback` block belong to the declined attempt: they are omitted
/// (the `fallback` block itself stays in place) and never executed.
pub fn sanitize_after_fallback(content: Vec<Value>) -> Vec<Value> {
    let Some(last_fallback) = content.iter().rposition(|b| block_type(b) == "fallback") else {
        return content;
    };
    let result_ids: Vec<String> = content
        .iter()
        .filter(|b| block_type(b).ends_with("_tool_result"))
        .filter_map(|b| b.get("tool_use_id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    content
        .into_iter()
        .enumerate()
        .filter(|(i, block)| {
            if *i >= last_fallback {
                return true;
            }
            match block_type(block) {
                "thinking" | "redacted_thinking" | "tool_use" => false,
                "server_tool_use" => {
                    block.get("id").and_then(Value::as_str).is_some_and(|id| result_ids.iter().any(|r| r == id))
                }
                _ => true,
            }
        })
        .map(|(_, b)| b)
        .collect()
}

pub fn block_type(block: &Value) -> &str {
    block.get("type").and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_declined_attempt_blocks_before_fallback() {
        let content = vec![
            json!({"type": "thinking", "thinking": "", "signature": "a"}),
            json!({"type": "text", "text": "partial"}),
            json!({"type": "tool_use", "id": "t0", "name": "read_file", "input": {}}),
            json!({"type": "fallback", "from": {"model": "x"}, "to": {"model": "y"}}),
            json!({"type": "thinking", "thinking": "", "signature": "b"}),
            json!({"type": "tool_use", "id": "t1", "name": "read_file", "input": {}}),
        ];
        let kept = sanitize_after_fallback(content);
        let types: Vec<&str> = kept.iter().map(block_type).collect();
        assert_eq!(types, vec!["text", "fallback", "thinking", "tool_use"]);
        assert_eq!(kept[3]["id"], "t1");
    }
}
