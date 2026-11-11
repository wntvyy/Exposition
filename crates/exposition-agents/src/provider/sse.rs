//! Server-sent events decoding and Messages API stream assembly.
//!
//! [`SseDecoder`] turns raw bytes into events (handling CRLF, multi-line data,
//! comments, and chunk boundaries anywhere). [`MessageAssembler`] rebuilds the
//! assistant message from `message_start` / `content_block_*` /
//! `message_delta` / `message_stop` events, preserving every content block
//! exactly (thinking signatures, fallback blocks, unknown block types) and
//! parsing streamed tool input strictly at `content_block_stop`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::{EventSink, ProviderError, ProviderResponse, StreamEvent, Usage, block_type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every event completed by them.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// Flush a final event not followed by a blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buffer.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            let rest = rest.trim_end_matches('\r').to_owned();
            if let Some(event) = self.line(&rest) {
                return Some(event);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event = None;
            return None;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        Some(SseEvent { event: self.event.take(), data })
    }
}

/// Rebuilds one streamed assistant message.
#[derive(Debug, Default)]
pub struct MessageAssembler {
    model: String,
    blocks: Vec<Option<Value>>,
    partial_json: HashMap<usize, String>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
    usage: Usage,
    invalid_tool_inputs: HashMap<String, String>,
    stopped: bool,
    /// Whether any user-visible output was emitted (text/thinking/tool start).
    pub emitted: bool,
}

fn protocol(message: impl Into<String>) -> ProviderError {
    ProviderError::Protocol(message.into())
}

impl MessageAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_complete(&self) -> bool {
        self.stopped
    }

    /// Handle one SSE event.
    pub fn handle(&mut self, event: &SseEvent, sink: EventSink<'_>) -> Result<(), ProviderError> {
        let data: Value =
            serde_json::from_str(&event.data).map_err(|e| protocol(format!("event data is not JSON: {e}")))?;
        let kind = data.get("type").and_then(Value::as_str).or(event.event.as_deref()).unwrap_or("");
        match kind {
            "message_start" => {
                let message = data.get("message").ok_or_else(|| protocol("message_start without message"))?;
                self.model = message.get("model").and_then(Value::as_str).unwrap_or_default().to_owned();
                if let Some(usage) = message.get("usage") {
                    merge_usage(&mut self.usage, usage);
                }
                sink(StreamEvent::MessageStart { model: self.model.clone() });
            }
            "content_block_start" => {
                let index = index_of(&data)?;
                let block = data.get("content_block").cloned().ok_or_else(|| protocol("block start without block"))?;
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, None);
                }
                match block_type(&block) {
                    "tool_use" | "server_tool_use" => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let name = block.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                        self.partial_json.insert(index, String::new());
                        self.emitted = true;
                        sink(StreamEvent::ToolUseStart { id, name });
                    }
                    "fallback" => {
                        let from = block.pointer("/from/model").and_then(Value::as_str).unwrap_or("?").to_owned();
                        let to = block.pointer("/to/model").and_then(Value::as_str).unwrap_or("?").to_owned();
                        sink(StreamEvent::Fallback { from, to });
                    }
                    "text" => {
                        if let Some(text) = block.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            self.emitted = true;
                            sink(StreamEvent::TextDelta(text.to_owned()));
                        }
                    }
                    _ => {}
                }
                self.blocks[index] = Some(block);
            }
            "content_block_delta" => {
                let index = index_of(&data)?;
                let delta = data.get("delta").ok_or_else(|| protocol("delta without body"))?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .and_then(Option::as_mut)
                    .ok_or_else(|| protocol(format!("delta for unknown block {index}")))?;
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                        append_str(block, "text", text);
                        self.emitted = true;
                        sink(StreamEvent::TextDelta(text.to_owned()));
                    }
                    "thinking_delta" => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or_default();
                        append_str(block, "thinking", text);
                        if !text.is_empty() {
                            self.emitted = true;
                            sink(StreamEvent::ThinkingDelta(text.to_owned()));
                        }
                    }
                    "signature_delta" => {
                        let sig = delta.get("signature").and_then(Value::as_str).unwrap_or_default();
                        append_str(block, "signature", sig);
                    }
                    "input_json_delta" => {
                        let part = delta.get("partial_json").and_then(Value::as_str).unwrap_or_default();
                        self.partial_json.entry(index).or_default().push_str(part);
                    }
                    "citations_delta" => {
                        if let (Some(obj), Some(citation)) = (block.as_object_mut(), delta.get("citation")) {
                            let list = obj.entry("citations").or_insert_with(|| json!([]));
                            if let Some(array) = list.as_array_mut() {
                                array.push(citation.clone());
                            }
                        }
                    }
                    _ => {
                        // Unknown delta kinds are ignored; the block is kept as started.
                    }
                }
            }
            "content_block_stop" => {
                let index = index_of(&data)?;
                if let Some(raw) = self.partial_json.remove(&index) {
                    let block = self
                        .blocks
                        .get_mut(index)
                        .and_then(Option::as_mut)
                        .ok_or_else(|| protocol(format!("stop for unknown block {index}")))?;
                    let parsed = if raw.trim().is_empty() {
                        Ok(Value::Object(Map::new()))
                    } else {
                        serde_json::from_str::<Value>(&raw)
                    };
                    match parsed {
                        Ok(value @ Value::Object(_)) => {
                            block["input"] = value;
                        }
                        _ => {
                            // Eager input streaming: the API does not validate the
                            // input, so malformed JSON is reported to the model as
                            // an error result instead of being run.
                            let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                            block["input"] = Value::Object(Map::new());
                            self.invalid_tool_inputs.insert(id, raw);
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = data.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(Value::as_str) {
                        self.stop_reason = Some(reason.to_owned());
                    }
                    if let Some(details) = delta.get("stop_details").filter(|d| !d.is_null()) {
                        self.stop_details = Some(details.clone());
                    }
                }
                if let Some(usage) = data.get("usage") {
                    merge_usage(&mut self.usage, usage);
                }
            }
            "message_stop" => {
                self.stopped = true;
            }
            "ping" => {}
            "error" => return Err(stream_error(&data)),
            _ => {}
        }
        Ok(())
    }

    pub fn finish(self) -> Result<ProviderResponse, ProviderError> {
        if !self.stopped {
            return Err(ProviderError::Network("the response stream ended before message_stop".into()));
        }
        let stop_reason = self.stop_reason.ok_or_else(|| protocol("message ended without stop_reason"))?;
        Ok(ProviderResponse {
            model: self.model,
            content: self.blocks.into_iter().flatten().collect(),
            stop_reason,
            stop_details: self.stop_details,
            usage: self.usage,
            invalid_tool_inputs: self.invalid_tool_inputs,
        })
    }
}

fn index_of(data: &Value) -> Result<usize, ProviderError> {
    data.get("index")
        .and_then(Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
        .filter(|&i| i < 4096)
        .ok_or_else(|| protocol("event without a valid index"))
}

fn append_str(block: &mut Value, field: &str, text: &str) {
    if let Some(obj) = block.as_object_mut() {
        let entry = obj.entry(field).or_insert_with(|| Value::String(String::new()));
        if let Value::String(existing) = entry {
            existing.push_str(text);
        } else {
            *entry = Value::String(text.to_owned());
        }
    }
}

/// Merge a `usage` object. With server-side fallbacks, `iterations` lists every
/// attempt; budgets count all of them.
fn merge_usage(usage: &mut Usage, value: &Value) {
    let get = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64);
    if let Some(iterations) = value.get("iterations").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let sum = |key: &str| iterations.iter().filter_map(|it| get(it, key)).sum::<u64>();
        usage.input_tokens = sum("input_tokens");
        usage.output_tokens = sum("output_tokens");
        usage.cache_read_tokens = sum("cache_read_input_tokens");
        usage.cache_write_tokens = sum("cache_creation_input_tokens");
        return;
    }
    if let Some(v) = get(value, "input_tokens") {
        usage.input_tokens = v;
    }
    if let Some(v) = get(value, "output_tokens") {
        usage.output_tokens = v;
    }
    if let Some(v) = get(value, "cache_read_input_tokens") {
        usage.cache_read_tokens = v;
    }
    if let Some(v) = get(value, "cache_creation_input_tokens") {
        usage.cache_write_tokens = v;
    }
}

/// Map an `error` body (`{"type":"error","error":{"type":...,"message":...}}`).
pub fn stream_error(data: &Value) -> ProviderError {
    let kind = data.pointer("/error/type").and_then(Value::as_str).unwrap_or("api_error");
    let message = bounded(data.pointer("/error/message").and_then(Value::as_str).unwrap_or(kind));
    match kind {
        "overloaded_error" => ProviderError::Overloaded(message),
        "rate_limit_error" => ProviderError::RateLimited { retry_after_ms: None, message },
        "authentication_error" | "permission_error" => ProviderError::Authentication(message),
        "invalid_request_error" | "not_found_error" | "request_too_large" | "billing_error" => {
            ProviderError::InvalidRequest(message)
        }
        _ => ProviderError::Server { status: 500, message },
    }
}

/// Bound provider-supplied text before it reaches logs or the UI.
pub fn bounded(text: &str) -> String {
    const LIMIT: usize = 600;
    if text.len() <= LIMIT {
        return text.to_owned();
    }
    let mut end = LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Recorded-shape stream: thinking (summarized) + text + two parallel tool
    /// calls, split at awkward byte boundaries with CRLF line endings.
    const FIXTURE: &str = concat!(
        "event: message_start\r\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1200,\"cache_creation_input_tokens\":900,\"cache_read_input_tokens\":0,\"output_tokens\":2}}}\r\n\r\n",
        ": keep-alive comment\r\n\r\n",
        "event: content_block_start\r\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Need both files.\"}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqQBCkgIBRAB\"}}\r\n\r\n",
        "event: content_block_stop\r\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\r\n\r\n",
        "event: content_block_start\r\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Reading \\u03b1 and \"}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"β files.\"}}\r\n\r\n",
        "event: content_block_stop\r\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\r\n\r\n",
        "event: content_block_start\r\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"read_file\",\"input\":{}}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pa\"}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"th\\\": \\\"main.typ\\\"}\"}}\r\n\r\n",
        "event: content_block_stop\r\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\r\n\r\n",
        "event: content_block_start\r\n",
        "data: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"read_file\",\"input\":{}}}\r\n\r\n",
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"refs.bib\\\"\"}}\r\n\r\n",
        "event: content_block_stop\r\n",
        "data: {\"type\":\"content_block_stop\",\"index\":3}\r\n\r\n",
        "event: message_delta\r\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null,\"stop_details\":null},\"usage\":{\"output_tokens\":87}}\r\n\r\n",
        "event: ping\r\n",
        "data: {\"type\": \"ping\"}\r\n\r\n",
        "event: message_stop\r\n",
        "data: {\"type\":\"message_stop\"}\r\n\r\n",
    );

    fn assemble(bytes: &[u8], chunk: usize) -> (Result<ProviderResponse, ProviderError>, Vec<StreamEvent>) {
        let events = Mutex::new(Vec::new());
        let sink = |e: StreamEvent| events.lock().unwrap().push(e);
        let mut decoder = SseDecoder::new();
        let mut assembler = MessageAssembler::new();
        for piece in bytes.chunks(chunk) {
            for event in decoder.push(piece) {
                if let Err(e) = assembler.handle(&event, &sink) {
                    return (Err(e), events.into_inner().unwrap());
                }
            }
        }
        if let Some(event) = decoder.finish() {
            assembler.handle(&event, &sink).unwrap();
        }
        (assembler.finish(), events.into_inner().unwrap())
    }

    #[test]
    fn assembles_recorded_stream_at_any_chunking() {
        for chunk in [1, 3, 7, 64, FIXTURE.len()] {
            let (response, events) = assemble(FIXTURE.as_bytes(), chunk);
            let response = response.unwrap();
            assert_eq!(response.model, "claude-opus-5-5");
            assert_eq!(response.stop_reason, "tool_use");
            assert_eq!(response.content.len(), 4);
            assert_eq!(response.content[0]["signature"], "EqQBCkgIBRAB");
            assert_eq!(response.content[0]["thinking"], "Need both files.");
            assert_eq!(response.content[1]["text"], "Reading α and β files.");
            assert_eq!(response.content[2]["input"], json!({"path": "main.typ"}));
            // The second tool input was cut off: invalid JSON is flagged, not run.
            assert_eq!(response.content[3]["input"], json!({}));
            assert_eq!(response.invalid_tool_inputs.get("toolu_b").unwrap(), "{\"path\": \"refs.bib\"");
            assert_eq!(
                response.usage,
                Usage { input_tokens: 1200, output_tokens: 87, cache_read_tokens: 0, cache_write_tokens: 900 }
            );
            let text: String = events
                .iter()
                .filter_map(|e| if let StreamEvent::TextDelta(t) = e { Some(t.as_str()) } else { None })
                .collect();
            assert_eq!(text, "Reading α and β files.");
            assert!(events.contains(&StreamEvent::ToolUseStart { id: "toolu_a".into(), name: "read_file".into() }));
        }
    }

    #[test]
    fn reports_refusal_with_stop_details_and_fallback_blocks() {
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"fallback\",\"from\":{\"model\":\"claude-opus-5-5\"},\"to\":{\"model\":\"claude-opus-5\"}}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\",\"explanation\":\"declined\"}},\"usage\":{\"output_tokens\":0,\"iterations\":[{\"type\":\"message\",\"input_tokens\":10,\"output_tokens\":0},{\"type\":\"fallback_message\",\"input_tokens\":12,\"output_tokens\":3}]}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (response, events) = assemble(stream.as_bytes(), 5);
        let response = response.unwrap();
        assert_eq!(response.stop_reason, "refusal");
        assert_eq!(response.stop_details.unwrap()["category"], "cyber");
        assert_eq!(response.usage.input_tokens, 22);
        assert_eq!(response.usage.output_tokens, 3);
        assert!(events.contains(&StreamEvent::Fallback { from: "claude-opus-5-5".into(), to: "claude-opus-5".into() }));
    }

    #[test]
    fn maps_error_events_and_truncated_streams() {
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{}}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        );
        let (response, _) = assemble(stream.as_bytes(), 9);
        assert_eq!(response.unwrap_err(), ProviderError::Overloaded("Overloaded".into()));
        assert!(response_is_retryable(&ProviderError::Overloaded(String::new())));

        let truncated = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\"}}\n\n";
        let (response, _) = assemble(truncated.as_bytes(), 4);
        assert!(matches!(response.unwrap_err(), ProviderError::Network(_)));
    }

    fn response_is_retryable(e: &ProviderError) -> bool {
        e.is_retryable()
    }

    #[test]
    fn decoder_handles_multiline_data_and_missing_final_blank_line() {
        let mut decoder = SseDecoder::new();
        let mut events = decoder.push(b"data: a\ndata: b\n\nevent: x\ndata: c");
        events.extend(decoder.finish());
        assert_eq!(
            events,
            vec![SseEvent { event: None, data: "a\nb".into() }, SseEvent { event: Some("x".into()), data: "c".into() }]
        );
    }
}
