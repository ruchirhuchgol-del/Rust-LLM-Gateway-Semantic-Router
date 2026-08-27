//! Incremental SSE Parser for proxying OpenAI-compatible streams.
//! Handles:
//! - Extracting `usage` from trailing `[DONE]` or `data:` lines.
//! - Accurately tracking TTFT based on the first *meaningful* content chunk,
//!   not just the first network byte.
//! - Handling partial `data:` lines split across network chunks.

use bytes::{BufMut, BytesMut};
use serde_json::Value;

pub struct SseParser {
    buffer: BytesMut,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(8192),
        }
    }

    /// Push a chunk of bytes from the network into the parser.
    /// Returns a list of complete SSE events found in the buffer.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.put_slice(chunk);
        let mut events = Vec::new();

        while let Some(idx) = self.find_event_end() {
            let event_bytes = self.buffer.split_to(idx + 2);

            if let Ok(event_str) = String::from_utf8(event_bytes.to_vec()) {
                events.push(event_str);
            }
        }

        events
    }

    fn find_event_end(&self) -> Option<usize> {
        self.buffer.windows(2).position(|w| w == b"\n\n")
    }

    /// Returns true when an SSE event contains an actual streamed
    /// content delta.
    ///
    /// This is used for TTFT measurement, so metadata-only events,
    /// usage events, role-only deltas, and [DONE] must not count.
    pub fn is_content_event(event: &str) -> bool {
        for line in event.lines() {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };

            if data == "[DONE]" {
                continue;
            }

            let Ok(val) = serde_json::from_str::<Value>(data) else {
                continue;
            };

            // OpenAI-compatible chat completion streaming format:
            //
            // {
            //   "choices": [{
            //     "delta": {
            //       "content": "Hello"
            //     }
            //   }]
            // }
            //
            // Only count a non-empty string content delta as actual
            // generated content for TTFT.
            if let Some(choices) = val.get("choices").and_then(Value::as_array) {
                for choice in choices {
                    if let Some(content) = choice
                        .get("delta")
                        .and_then(|delta| delta.get("content"))
                        .and_then(Value::as_str)
                    {
                        if !content.is_empty() {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    /// Helper to extract prompt/completion tokens from a single SSE event string.
    pub fn extract_usage(event: &str) -> Option<(u64, u64)> {
        for line in event.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    continue;
                }

                if let Ok(val) = serde_json::from_str::<Value>(data) {
                    if let Some(usage) = val.get("usage") {
                        let prompt = usage
                            .get("prompt_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);

                        let completion = usage
                            .get("completion_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);

                        return Some((prompt, completion));
                    }
                }
            }
        }

        None
    }
}
