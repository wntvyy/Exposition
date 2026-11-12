//! Kernel host helper protocol (version [`crate::helper::PROTOCOL_VERSION`]).
//! See `workers/kernel-host/exposition_kernel_host.py` for the full description.

use serde::Deserialize;
use serde_json::Value;

/// A line from the helper.
#[derive(Debug, Clone, PartialEq)]
pub enum HelperMessage {
    Response {
        id: u64,
        result: Result<Value, HelperFailure>,
    },
    Event(HelperEvent),
    /// A well-formed line this supervisor does not understand (ignored).
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HelperFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperFile {
    pub path: String,
    pub name: String,
    pub media_type: String,
    /// `image`, `html`, or `table`.
    pub role: String,
    pub size: u64,
    pub sha256: String,
    pub seq: u32,
    /// A table grid that was cut to the row/column limits.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum HelperEvent {
    Heartbeat {
        seq: u64,
    },
    KernelStatus {
        state: String,
    },
    RunStarted {
        #[serde(rename = "run_id")]
        run_id: String,
        execution_count: Option<u32>,
    },
    Stream {
        #[serde(rename = "run_id")]
        run_id: String,
        name: String,
        text: String,
    },
    Display {
        #[serde(rename = "run_id")]
        run_id: String,
        output_type: String,
        execution_count: Option<u32>,
        text: Option<String>,
        #[serde(default)]
        files: Vec<HelperFile>,
    },
    ClearOutput {
        #[serde(rename = "run_id")]
        run_id: String,
        #[serde(default)]
        wait: bool,
    },
    Error {
        #[serde(rename = "run_id")]
        run_id: String,
        ename: String,
        evalue: String,
        #[serde(default)]
        traceback: Vec<String>,
    },
    RunFinished {
        #[serde(rename = "run_id")]
        run_id: String,
        /// `ok`, `error`, `aborted`, or `lost` (kernel died/restarted/shut down).
        status: String,
        #[serde(default)]
        reason: Option<String>,
        execution_count: Option<u32>,
        #[serde(default)]
        stream_dropped: u64,
        #[serde(default)]
        files_dropped: u32,
    },
    KernelDied {
        exit_code: Option<i64>,
        #[serde(default)]
        runs: Vec<String>,
    },
    Log {
        level: String,
        message: String,
    },
}

#[derive(Deserialize)]
struct RawResponse {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<HelperFailure>,
}

/// Parse one protocol line. Returns `None` for malformed lines.
pub fn parse_line(line: &str) -> Option<HelperMessage> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str) {
        Some("response") => {
            let raw: RawResponse = serde_json::from_value(value).ok()?;
            let result = if raw.ok {
                Ok(raw.result)
            } else {
                Err(raw.error.unwrap_or(HelperFailure {
                    code: "helper.unknown".into(),
                    message: "the helper reported an unspecified failure".into(),
                }))
            };
            Some(HelperMessage::Response { id: raw.id, result })
        }
        Some("event") => match serde_json::from_value::<HelperEvent>(value) {
            Ok(event) => Some(HelperMessage::Event(event)),
            Err(_) => Some(HelperMessage::Unknown(line.chars().take(200).collect())),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_responses_and_events() {
        let ok = parse_line(r#"{"type":"response","id":3,"ok":true,"result":{"msgId":"m"}}"#).unwrap();
        assert_eq!(ok, HelperMessage::Response { id: 3, result: Ok(serde_json::json!({"msgId": "m"})) });
        let failed =
            parse_line(r#"{"type":"response","id":4,"ok":false,"error":{"code":"kernel.dead","message":"x"}}"#)
                .unwrap();
        assert!(matches!(failed, HelperMessage::Response { id: 4, result: Err(HelperFailure { .. }) }));
        let stream =
            parse_line(r#"{"type":"event","event":"stream","run_id":"r1","name":"stdout","text":"hi\n"}"#).unwrap();
        assert_eq!(
            stream,
            HelperMessage::Event(HelperEvent::Stream {
                run_id: "r1".into(),
                name: "stdout".into(),
                text: "hi\n".into()
            })
        );
        let finished = parse_line(
            r#"{"type":"event","event":"run_finished","run_id":"r1","status":"ok","executionCount":2,"streamDropped":0,"filesDropped":0}"#,
        )
        .unwrap();
        assert!(matches!(
            finished,
            HelperMessage::Event(HelperEvent::RunFinished { execution_count: Some(2), ref status, .. }) if status == "ok"
        ));
        assert!(matches!(parse_line(r#"{"type":"event","event":"future"}"#), Some(HelperMessage::Unknown(_))));
        assert!(parse_line("not json").is_none());
    }
}
