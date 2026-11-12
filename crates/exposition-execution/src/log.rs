//! Bounded, redacted job output.
//!
//! Raw bytes from a child stream are decoded (lossy UTF-8, split multi-byte
//! sequences carried across reads), split into complete lines, redacted, and
//! queued for emission. The service flushes queued text at a bounded rate:
//! each flush becomes one [`OutputChunk`] with a sequence number shared by both
//! streams of a job, is appended to that stream's ring buffer, and is emitted
//! as one `JobEvent::Output`. A ring buffer keeps at most `limit` bytes per
//! stream, dropping the oldest output first.
//!
//! Redaction happens on complete lines. A partial line is released only when
//! it grows large, when the stream is idle, or at end of stream; in the first
//! two cases a trailing fragment that could be the start of a secret is held
//! back (see [`Redactor::safe_release_len`]).

use std::collections::VecDeque;

use exposition_core::job::OutputStream;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::credentials::{Redactor, replace_ranges};

/// Default retained bytes per stream.
pub const DEFAULT_LOG_LIMIT: usize = 1024 * 1024;
/// Bounds for a job-requested log limit.
pub const MIN_LOG_LIMIT: usize = 1024;
pub const MAX_LOG_LIMIT: usize = 64 * 1024 * 1024;
/// A partial line longer than this is released (minus a held-back tail).
const MAX_PARTIAL: usize = 16 * 1024;
/// Recently released raw text kept as context, so a secret split across two
/// releases (e.g. by an idle pause mid-token) is still found and its
/// not-yet-emitted part redacted.
const RELEASED_CONTEXT: usize = 1024;

/// One emitted piece of output. `seq` orders chunks across both streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OutputChunk {
    pub seq: u64,
    pub stream: OutputStream,
    pub text: String,
}

/// A page of retained output, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OutputPage {
    pub chunks: Vec<OutputChunk>,
    /// More retained chunks follow the last one returned.
    pub has_more: bool,
    /// Output was dropped (log limit) before the oldest retained chunk.
    pub truncated: bool,
    /// The job has finished; no more output will arrive.
    pub complete: bool,
}

/// Per-stream ring buffer of chunks.
#[derive(Debug)]
pub struct RingLog {
    limit: usize,
    bytes: usize,
    chunks: VecDeque<OutputChunk>,
    truncated: bool,
}

impl RingLog {
    pub fn new(limit: usize) -> Self {
        Self { limit: limit.clamp(MIN_LOG_LIMIT, MAX_LOG_LIMIT), bytes: 0, chunks: VecDeque::new(), truncated: false }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    pub fn push(&mut self, mut chunk: OutputChunk) {
        if chunk.text.len() > self.limit {
            let cut = floor_char_boundary(&chunk.text, chunk.text.len() - self.limit);
            let cut = ceil_to_boundary(&chunk.text, cut);
            chunk.text.drain(..cut);
            self.truncated = true;
        }
        self.bytes += chunk.text.len();
        self.chunks.push_back(chunk);
        while self.bytes > self.limit {
            match self.chunks.pop_front() {
                Some(old) => {
                    self.bytes -= old.text.len();
                    self.truncated = true;
                }
                None => break,
            }
        }
    }

    pub fn chunks(&self) -> impl Iterator<Item = &OutputChunk> {
        self.chunks.iter()
    }

    pub fn text(&self) -> String {
        self.chunks.iter().map(|c| c.text.as_str()).collect()
    }
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    index = index.min(s.len());
    while !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_to_boundary(s: &str, mut index: usize) -> usize {
    while index < s.len() && !s.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Merge both streams' retained chunks into a page after `after` (exclusive).
pub fn page(stdout: &RingLog, stderr: &RingLog, after: Option<u64>, max_bytes: usize, complete: bool) -> OutputPage {
    let mut all: Vec<&OutputChunk> =
        stdout.chunks().chain(stderr.chunks()).filter(|c| after.is_none_or(|a| c.seq > a)).collect();
    all.sort_by_key(|c| c.seq);
    let mut chunks = Vec::new();
    let mut used = 0usize;
    let mut has_more = false;
    for chunk in all {
        if !chunks.is_empty() && used + chunk.text.len() > max_bytes {
            has_more = true;
            break;
        }
        used += chunk.text.len();
        chunks.push(chunk.clone());
    }
    OutputPage { chunks, has_more, truncated: stdout.truncated() || stderr.truncated(), complete }
}

/// Decodes and redacts one stream and queues text for emission.
#[derive(Debug)]
pub struct StreamCapture {
    /// Incomplete UTF-8 sequence carried to the next read.
    carry: Vec<u8>,
    /// Unredacted text not yet released (no trailing newline yet).
    partial: String,
    /// Tail of recently released raw text (context for cross-release redaction).
    released: String,
    /// Redacted text awaiting emission.
    ready: String,
    /// Upper bound for `ready`; excess oldest text is dropped.
    ready_cap: usize,
    dropped: bool,
}

impl StreamCapture {
    pub fn new(limit: usize) -> Self {
        Self {
            carry: Vec::new(),
            partial: String::new(),
            released: String::new(),
            ready: String::new(),
            ready_cap: limit,
            dropped: false,
        }
    }

    /// Feed raw bytes from the child.
    pub fn push_bytes(&mut self, bytes: &[u8], redactor: &Redactor) {
        let text = self.decode(bytes);
        self.partial.push_str(&text);
        if let Some(pos) = self.partial.rfind('\n') {
            let complete: String = self.partial.drain(..=pos).collect();
            let redacted = self.redact_release(&complete, redactor);
            self.enqueue(&redacted);
        }
        if self.partial.len() > MAX_PARTIAL {
            self.release_partial(redactor, false);
        }
    }

    /// Release a partial line: everything when `force` (end of stream),
    /// otherwise all but a possible secret prefix at the end.
    pub fn release_partial(&mut self, redactor: &Redactor, force: bool) {
        if force && !self.carry.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned();
            self.partial.push_str(&rest);
        }
        if self.partial.is_empty() {
            return;
        }
        let cut = if force { self.partial.len() } else { redactor.safe_release_len(&self.partial) };
        if cut == 0 {
            return;
        }
        let released: String = self.partial.drain(..cut).collect();
        let redacted = self.redact_release(&released, redactor);
        self.enqueue(&redacted);
    }

    pub fn has_partial(&self) -> bool {
        !self.partial.is_empty()
    }

    /// Take redacted text for emission.
    pub fn take_ready(&mut self) -> Option<String> {
        if self.ready.is_empty() { None } else { Some(std::mem::take(&mut self.ready)) }
    }

    /// True if queued output was dropped because emission fell behind.
    pub fn take_dropped(&mut self) -> bool {
        std::mem::take(&mut self.dropped)
    }

    /// Redact `text` for release, also catching secrets that began in text
    /// released earlier: matches are searched in `released + text`, and the
    /// part of each match that lies in `text` is replaced.
    fn redact_release(&mut self, text: &str, redactor: &Redactor) -> String {
        let context = self.released.len();
        let mut combined = std::mem::take(&mut self.released);
        combined.push_str(text);
        let ranges: Vec<(usize, usize)> = redactor
            .secret_ranges(&combined)
            .into_iter()
            .filter(|&(_, end)| end > context)
            .map(|(start, end)| (start.max(context) - context, end - context))
            .collect();
        let keep_from = ceil_to_boundary(&combined, combined.len().saturating_sub(RELEASED_CONTEXT));
        self.released = combined[keep_from..].to_owned();
        replace_ranges(text, &ranges)
    }

    fn enqueue(&mut self, text: &str) {
        self.ready.push_str(text);
        if self.ready.len() > self.ready_cap {
            // Drop the oldest queued text, preferably at a line boundary. The
            // offset is moved to a char boundary before slicing.
            let excess = ceil_to_boundary(&self.ready, self.ready.len() - self.ready_cap);
            let cut = match self.ready[excess..].find('\n') {
                Some(offset) => excess + offset + 1,
                None => excess,
            };
            self.ready.drain(..cut);
            self.dropped = true;
        }
    }

    fn decode(&mut self, bytes: &[u8]) -> String {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(bytes);
        // Keep an incomplete trailing UTF-8 sequence for the next read.
        let mut end = data.len();
        let start = data.len().saturating_sub(3);
        for i in (start..data.len()).rev() {
            let byte = data[i];
            if byte & 0b1100_0000 == 0b1000_0000 {
                continue; // continuation byte
            }
            let needed = if byte >= 0b1111_0000 {
                4
            } else if byte >= 0b1110_0000 {
                3
            } else if byte >= 0b1100_0000 {
                2
            } else {
                1
            };
            if data.len() - i < needed {
                end = i;
            }
            break;
        }
        self.carry = data.split_off(end);
        String::from_utf8_lossy(&data).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::REDACTED as REDACTED_MARK;

    fn chunk(seq: u64, stream: OutputStream, text: &str) -> OutputChunk {
        OutputChunk { seq, stream, text: text.into() }
    }

    #[test]
    fn ring_log_keeps_newest_within_limit() {
        let mut log = RingLog::new(MIN_LOG_LIMIT);
        for i in 0..100 {
            log.push(chunk(i, OutputStream::Stdout, &format!("{:0>99}\n", i)));
        }
        assert!(log.bytes() <= MIN_LOG_LIMIT);
        assert!(log.truncated());
        assert!(log.text().ends_with(&format!("{:0>99}\n", 99)));
        // A single oversized chunk keeps its newest bytes.
        let mut log = RingLog::new(MIN_LOG_LIMIT);
        let big = "é".repeat(MIN_LOG_LIMIT);
        log.push(chunk(1, OutputStream::Stderr, &big));
        assert!(log.bytes() <= MIN_LOG_LIMIT && log.bytes() > 0);
        assert!(log.truncated());
    }

    #[test]
    fn page_merges_streams_in_sequence() {
        let mut out = RingLog::new(MIN_LOG_LIMIT);
        let mut err = RingLog::new(MIN_LOG_LIMIT);
        out.push(chunk(1, OutputStream::Stdout, "a"));
        err.push(chunk(2, OutputStream::Stderr, "b"));
        out.push(chunk(3, OutputStream::Stdout, "c"));
        let p = page(&out, &err, None, 1024, false);
        assert_eq!(p.chunks.iter().map(|c| c.text.as_str()).collect::<String>(), "abc");
        assert_eq!(p.chunks[1].stream, OutputStream::Stderr);
        let p = page(&out, &err, Some(1), 1, true);
        assert_eq!(p.chunks.len(), 1);
        assert_eq!(p.chunks[0].seq, 2);
        assert!(p.has_more && p.complete && !p.truncated);
    }

    #[test]
    fn capture_redacts_complete_lines_and_secrets_split_across_reads() {
        let redactor = Redactor::from_host_env([("MY_SECRET", "hunter2-is-the-password")]);
        let mut cap = StreamCapture::new(DEFAULT_LOG_LIMIT);
        cap.push_bytes(b"value=hunter2-is", &redactor);
        assert_eq!(cap.take_ready(), None, "partial line held");
        cap.release_partial(&redactor, false);
        assert_eq!(cap.take_ready().as_deref(), Some("value="), "secret prefix held back while idle");
        cap.push_bytes(b"-the-password\nnext", &redactor);
        assert_eq!(cap.take_ready().as_deref(), Some("[REDACTED]\n"));
        cap.release_partial(&redactor, true);
        assert_eq!(cap.take_ready().as_deref(), Some("next"));
    }

    #[test]
    fn capture_decodes_split_utf8() {
        let redactor = Redactor::default();
        let mut cap = StreamCapture::new(DEFAULT_LOG_LIMIT);
        let bytes = "αβ\n".as_bytes();
        cap.push_bytes(&bytes[..1], &redactor);
        cap.push_bytes(&bytes[1..3], &redactor);
        cap.push_bytes(&bytes[3..], &redactor);
        assert_eq!(cap.take_ready().as_deref(), Some("αβ\n"));
        cap.push_bytes(&[0xff, b'\n'], &redactor);
        assert_eq!(cap.take_ready().as_deref(), Some("\u{fffd}\n"));
    }

    /// Regression (review R-m1): dropping queued text sliced at a byte offset
    /// inside a multi-byte character panicked and wedged the job.
    #[test]
    fn queued_multibyte_output_over_the_limit_is_bounded_without_panicking() {
        let redactor = Redactor::default();
        for cap in [1024, 1025, 1026] {
            let mut capture = StreamCapture::new(cap);
            let text = format!("{}\n", "数学".repeat(2_000));
            capture.push_bytes(text.as_bytes(), &redactor);
            capture.push_bytes("\u{fffd}".repeat(3_000).as_bytes(), &redactor);
            capture.release_partial(&redactor, true);
            let ready = capture.take_ready().unwrap();
            assert!(ready.len() <= cap, "cap {cap}: {} bytes", ready.len());
            assert!(capture.take_dropped());
        }
    }

    /// Regression (review R-m1): a secret written as "sk" + pause + the rest
    /// was released in two pieces and the body escaped redaction.
    #[test]
    fn secret_split_by_an_idle_pause_is_still_redacted() {
        let key = "sk-ant-api03-FAKEFAKEFAKE-test-only-0123456789";
        let redactor = Redactor::from_host_env([("ANTHROPIC_API_KEY", key)]);
        for split in [1, 2, 3, 6, 10, 20] {
            let mut capture = StreamCapture::new(DEFAULT_LOG_LIMIT);
            let (head, tail) = key.split_at(split);
            capture.push_bytes(format!("key={head}").as_bytes(), &redactor);
            capture.release_partial(&redactor, false); // idle flush
            let mut out = capture.take_ready().unwrap_or_default();
            capture.push_bytes(format!("{tail}\n").as_bytes(), &redactor);
            out.push_str(&capture.take_ready().unwrap_or_default());
            assert!(!out.contains("FAKEFAKE"), "split at {split}: {out:?}");
            assert!(out.contains(REDACTED_MARK), "split at {split}: {out:?}");
        }
        // Pattern-only tokens (not in the host environment) too.
        let plain = Redactor::default();
        let mut capture = StreamCapture::new(DEFAULT_LOG_LIMIT);
        capture.push_bytes(b"token gh", &plain);
        capture.release_partial(&plain, false);
        let mut out = capture.take_ready().unwrap_or_default();
        capture.push_bytes(b"p_abcdefghijklmnopqrstuvwxyz0123456789 end\n", &plain);
        out.push_str(&capture.take_ready().unwrap_or_default());
        assert!(!out.contains("abcdefghijklmnop"), "{out:?}");
    }

    #[test]
    fn capture_bounds_queued_text() {
        let redactor = Redactor::default();
        let mut cap = StreamCapture::new(MIN_LOG_LIMIT);
        for _ in 0..100 {
            cap.push_bytes(&[b'x'; 99], &redactor);
            cap.push_bytes(b"\n", &redactor);
        }
        let ready = cap.take_ready().unwrap();
        assert!(ready.len() <= MIN_LOG_LIMIT, "{}", ready.len());
        assert!(cap.take_dropped());
    }
}
