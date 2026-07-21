use bytes::Bytes;
use serde_json::Value;

const TOKENS_PER_MESSAGE: u64 = 4;

#[must_use]
pub(crate) fn estimate_input_tokens(body: &[u8]) -> u64 {
    let Ok(document) = serde_json::from_slice::<Value>(body) else {
        return 0;
    };
    let Some(object) = document.as_object() else {
        return 0;
    };
    let encoding = tiktoken_rs::o200k_base_singleton();
    let mut estimate = 0_u64;

    if let Some(system) = object.get("system") {
        estimate = estimate.saturating_add(count_system_tokens(encoding, system));
    }
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        for message in messages {
            estimate = estimate.saturating_add(TOKENS_PER_MESSAGE);
            if let Some(content) = message.get("content") {
                estimate = estimate.saturating_add(count_message_content(encoding, content));
            }
        }
    }
    if let Some(tools @ Value::Array(_)) = object.get("tools") {
        estimate = estimate.saturating_add(count_serialized_tokens(encoding, tools));
    }

    estimate
}

fn count_system_tokens(encoding: &tiktoken_rs::CoreBPE, system: &Value) -> u64 {
    match system {
        Value::String(text) => count_text_tokens(encoding, text),
        Value::Array(blocks) => blocks.iter().fold(0_u64, |total, block| {
            total.saturating_add(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map_or(0, |text| count_text_tokens(encoding, text)),
            )
        }),
        _ => 0,
    }
}

fn count_message_content(encoding: &tiktoken_rs::CoreBPE, content: &Value) -> u64 {
    match content {
        Value::String(text) => count_text_tokens(encoding, text),
        Value::Array(blocks) => blocks.iter().fold(0_u64, |total, block| {
            let block_tokens = match block.get("type").and_then(Value::as_str) {
                Some("tool_use" | "tool_result") => count_serialized_tokens(encoding, block),
                _ => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map_or(0, |text| count_text_tokens(encoding, text)),
            };
            total.saturating_add(block_tokens)
        }),
        _ => 0,
    }
}

fn count_text_tokens(encoding: &tiktoken_rs::CoreBPE, text: &str) -> u64 {
    u64::try_from(encoding.encode_ordinary(text).len()).unwrap_or(u64::MAX)
}

fn count_serialized_tokens(encoding: &tiktoken_rs::CoreBPE, value: &Value) -> u64 {
    serde_json::to_string(value)
        .ok()
        .map_or(0, |text| count_text_tokens(encoding, &text))
}

pub(crate) struct SseUsageTransformer {
    buffer: Vec<u8>,
    estimated_input_tokens: u64,
}

impl SseUsageTransformer {
    pub(crate) const fn new(estimated_input_tokens: u64) -> Self {
        Self {
            buffer: Vec::new(),
            estimated_input_tokens,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(event_end) = event_boundary(&self.buffer) {
            let event = self.buffer.drain(..event_end).collect::<Vec<_>>();
            output.push(Bytes::from(transform_event(
                event,
                self.estimated_input_tokens,
            )));
        }
        output
    }

    pub(crate) fn finish(self) -> Option<Bytes> {
        (!self.buffer.is_empty()).then(|| Bytes::from(self.buffer))
    }
}

fn event_boundary(buffer: &[u8]) -> Option<usize> {
    (0..buffer.len()).find_map(|index| {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            Some(index + 4)
        } else if buffer[index..].starts_with(b"\n\n") {
            Some(index + 2)
        } else {
            None
        }
    })
}

fn transform_event(event: Vec<u8>, estimated_input_tokens: u64) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(&event) else {
        return event;
    };
    let Some((event_name, data_range)) = event_fields(text) else {
        return event;
    };
    if event_name != "message_start" && event_name != "message_delta" {
        return event;
    }
    let Ok(mut data) = serde_json::from_str::<Value>(&text[data_range.clone()]) else {
        return event;
    };

    if event_name == "message_delta" {
        log_actual_usage(&data, estimated_input_tokens);
        return event;
    }

    let Some(input_tokens) = data
        .get_mut("message")
        .and_then(|message| message.get_mut("usage"))
        .and_then(|usage| usage.get_mut("input_tokens"))
    else {
        return event;
    };
    if input_tokens.as_u64() != Some(0) {
        return event;
    }
    *input_tokens = Value::from(estimated_input_tokens);

    let Ok(rewritten_data) = serde_json::to_vec(&data) else {
        return event;
    };
    let mut rewritten = Vec::with_capacity(
        event
            .len()
            .saturating_sub(data_range.len())
            .saturating_add(rewritten_data.len()),
    );
    rewritten.extend_from_slice(&event[..data_range.start]);
    rewritten.extend_from_slice(&rewritten_data);
    rewritten.extend_from_slice(&event[data_range.end..]);
    rewritten
}

fn event_fields(event: &str) -> Option<(&str, std::ops::Range<usize>)> {
    let mut event_name = None;
    let mut data_range = None;
    let mut offset = 0;

    for line_with_ending in event.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            if data_range.is_some() {
                return None;
            }
            let leading_whitespace = value.len().saturating_sub(value.trim_start().len());
            let trailing_whitespace = value.len().saturating_sub(value.trim_end().len());
            let start = offset + "data:".len() + leading_whitespace;
            let end = offset + line.len() - trailing_whitespace;
            data_range = Some(start..end);
        }
        offset += line_with_ending.len();
    }

    Some((event_name?, data_range?))
}

fn log_actual_usage(data: &Value, estimated: u64) {
    let Some(usage) = data.get("usage") else {
        return;
    };
    let Some(actual) = usage.get("input_tokens").and_then(Value::as_u64) else {
        return;
    };
    if let Some(cache_read) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
        tracing::debug!(
            estimated,
            actual,
            cache_read,
            "GPT input token estimate calibration"
        );
    } else {
        tracing::debug!(estimated, actual, "GPT input token estimate calibration");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transformed(chunks: &[&[u8]], estimate: u64) -> Vec<u8> {
        let mut transformer = SseUsageTransformer::new(estimate);
        let mut output = Vec::new();
        for chunk in chunks {
            for bytes in transformer.push(chunk) {
                output.extend_from_slice(&bytes);
            }
        }
        if let Some(bytes) = transformer.finish() {
            output.extend_from_slice(&bytes);
        }
        output
    }

    #[test]
    fn split_across_chunks_events_are_reassembled_without_corrupting_framing() {
        let output = transformed(
            &[
                b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":",
                b"0,\"output_tokens\":0}}}\n\nevent: ping\nda",
                b"ta: {}\n\n",
            ],
            37,
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""input_tokens":37"#));
        assert!(text.ends_with("event: ping\ndata: {}\n\n"));
    }

    #[test]
    fn message_start_zero_input_tokens_is_rewritten() {
        let output = transformed(
            &[b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n"],
            1234,
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""input_tokens":1234"#));
    }

    #[test]
    fn malformed_event_passes_through_unmodified() {
        let input = b"event: message_start\ndata: {not json}\n\n";
        assert_eq!(transformed(&[input], 99), input);
    }

    #[test]
    fn nonzero_message_start_input_tokens_is_not_rewritten() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":0}}}\n\n";
        assert_eq!(transformed(&[input], 99), input);
    }

    #[test]
    fn estimate_counts_system_messages_tools_and_message_overhead() {
        let full = estimate_input_tokens(
            br#"{"system":[{"type":"text","text":"system words"}],"messages":[{"role":"user","content":"hello"},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/x"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"contents"}]}],"tools":[{"name":"Read","description":"Read a file","input_schema":{"type":"object"}}]}"#,
        );
        let empty = estimate_input_tokens(br#"{"messages":[]}"#);
        assert!(full > empty + 3 * TOKENS_PER_MESSAGE);
    }
}
