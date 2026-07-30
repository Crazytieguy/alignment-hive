use bytes::Bytes;
use serde_json::Value;

use crate::client_window::UsageScale;

const TOKENS_PER_MESSAGE: u64 = 4;

/// The usage fields Claude Code's auto-compact gate sums (verified in the
/// 2.1.220 bundle: `input_tokens + cache_creation_input_tokens +
/// cache_read_input_tokens + output_tokens` of the most recent message that
/// carries usage). Scaling exactly these moves the compaction point.
///
/// Deliberately not a recursive walk of the usage object: sibling fields like
/// `server_tool_use.web_search_requests` are request counts, not tokens.
const SCALED_USAGE_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// Rewrites a `usage` object into the client's coordinate system. Absent
/// fields stay absent. Returns whether anything was actually rewritten, so an
/// event that gains nothing is forwarded byte-identical rather than
/// re-serialized.
fn scale_usage(usage: &mut Value, scale: UsageScale) -> bool {
    let Some(object) = usage.as_object_mut() else {
        return false;
    };
    let mut rewritten = false;
    for field in SCALED_USAGE_FIELDS {
        if let Some(value) = object.get_mut(field)
            && let Some(tokens) = value.as_u64()
        {
            *value = Value::from(scale.apply(tokens));
            rewritten = true;
        }
    }
    rewritten
}

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

/// How one routed request's token usage is reported back to Claude Code.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UsagePolicy {
    /// Stands in for the `input_tokens: 0` that `CLIProxyAPI` reports at
    /// `message_start`.
    pub(crate) estimate: u64,
    /// Set when the route's real context window differs from the one Claude
    /// Code believes it has.
    pub(crate) scale: Option<UsageScale>,
}

/// Everything the GPT branch rewrites in one request's response: usage
/// reporting, plus (for routes where it is known-correct) the
/// context-overflow error translation.
#[derive(Clone, Debug)]
pub(crate) struct GptPolicies {
    pub(crate) usage: UsagePolicy,
    pub(crate) overflow: Option<crate::overflow::OverflowRewrite>,
}

pub(crate) struct SseUsageTransformer {
    buffer: Vec<u8>,
    policies: GptPolicies,
}

impl SseUsageTransformer {
    pub(crate) const fn new(policies: GptPolicies) -> Self {
        Self {
            buffer: Vec::new(),
            policies,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(event_end) = event_boundary(&self.buffer) {
            let event = self.buffer.drain(..event_end).collect::<Vec<_>>();
            output.push(Bytes::from(transform_event(event, &self.policies)));
        }
        output
    }

    pub(crate) fn finish(self) -> Option<Bytes> {
        (!self.buffer.is_empty()).then(|| Bytes::from(self.buffer))
    }
}

pub(crate) fn event_boundary(buffer: &[u8]) -> Option<usize> {
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

fn transform_event(event: Vec<u8>, policies: &GptPolicies) -> Vec<u8> {
    let UsagePolicy { estimate, scale } = policies.usage;
    let Ok(text) = std::str::from_utf8(&event) else {
        return event;
    };
    let Some((event_name, data_range)) = event_fields(text) else {
        return event;
    };
    if event_name != "message_start" && event_name != "message_delta" && event_name != "error" {
        return event;
    }
    let Ok(mut data) = serde_json::from_str::<Value>(&text[data_range.clone()]) else {
        return event;
    };

    let changed = if event_name == "error" {
        // A streamed request that overflows the backend's context window
        // fails as an in-stream error event on a 200 response; translate it
        // the same way the buffered 400 path does.
        policies
            .overflow
            .as_ref()
            .is_some_and(|overflow| overflow.rewrite_envelope(&mut data))
    } else {
        // The two usage-bearing event shapes differ only in where the usage
        // object lives.
        let usage = if event_name == "message_delta" {
            // Calibration logging must see the upstream's own numbers, so it
            // runs before any scaling.
            log_actual_usage(&data, estimate);
            data.get_mut("usage")
        } else {
            data.get_mut("message")
                .and_then(|message| message.get_mut("usage"))
        };
        match usage {
            Some(usage) => {
                let injected =
                    event_name == "message_start" && inject_estimated_input_tokens(usage, estimate);
                let scaled = scale.is_some_and(|scale| scale_usage(usage, scale));
                injected || scaled
            }
            None => false,
        }
    };
    if !changed {
        return event;
    }

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

/// Substitutes the router's estimate for the `input_tokens: 0` that
/// `CLIProxyAPI` reports before the upstream has counted anything (`OpenAI`
/// streaming only reports usage in its final chunk). Claude Code seeds its
/// running total from that first number, so leaving it at zero would strand
/// the context meter. Returns whether it rewrote anything.
pub(crate) fn inject_estimated_input_tokens(usage: &mut Value, estimate: u64) -> bool {
    let Some(input_tokens) = usage.get_mut("input_tokens") else {
        return false;
    };
    if input_tokens.as_u64() != Some(0) {
        return false;
    }
    *input_tokens = Value::from(estimate);
    true
}

pub(crate) fn event_fields(event: &str) -> Option<(&str, std::ops::Range<usize>)> {
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
        transformed_with(chunks, estimate, None)
    }

    fn transformed_with(chunks: &[&[u8]], estimate: u64, scale: Option<UsageScale>) -> Vec<u8> {
        transformed_policies(
            chunks,
            GptPolicies {
                usage: UsagePolicy { estimate, scale },
                overflow: None,
            },
        )
    }

    fn transformed_policies(chunks: &[&[u8]], policies: GptPolicies) -> Vec<u8> {
        let mut transformer = SseUsageTransformer::new(policies);
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

    /// The in-stream error shape captured live from `CLIProxyAPI` 7.2.92: a
    /// streamed overflow fails as `event: error` on a 200 response.
    const OVERFLOW_ERROR_EVENT: &[u8] = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Your input exceeds the context window of this model. Please adjust your input and try again.\"}}\n\n";

    fn overflow_policies(estimate: u64) -> GptPolicies {
        GptPolicies {
            usage: UsagePolicy {
                estimate,
                scale: None,
            },
            overflow: Some(crate::overflow::OverflowRewrite::new(
                258_400,
                crate::overflow::Estimate::Computed(estimate),
            )),
        }
    }

    #[test]
    fn in_stream_overflow_error_is_rewritten_and_neighbors_pass_through() {
        let mut chunks: Vec<&[u8]> = vec![
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n",
        ];
        chunks.push(OVERFLOW_ERROR_EVENT);
        let output = transformed_policies(&chunks, overflow_policies(300_000));
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""input_tokens":300000"#));
        assert!(text.contains("prompt is too long: 300000 tokens > 258400 maximum"));
        assert!(!text.contains("exceeds the context window"));
    }

    #[test]
    fn in_stream_unrelated_error_passes_through_byte_identical() {
        let input: &[u8] = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Our servers are currently overloaded.\"}}\n\n";
        assert_eq!(
            transformed_policies(&[input], overflow_policies(300_000)),
            input
        );
    }

    #[test]
    fn without_an_overflow_rewrite_error_events_pass_through() {
        assert_eq!(
            transformed(&[OVERFLOW_ERROR_EVENT], 300_000),
            OVERFLOW_ERROR_EVENT
        );
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

    /// A real 1M-token window reported into a believed 250K one.
    fn quarter_scale() -> Option<UsageScale> {
        UsageScale::new(250_000, 1_000_000)
    }

    #[test]
    fn scaling_applies_to_an_injected_message_start_estimate() {
        let output = transformed_with(
            &[b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n"],
            4000,
            quarter_scale(),
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""input_tokens":1000"#), "{text}");
    }

    #[test]
    fn scaling_applies_when_upstream_reports_real_input_tokens() {
        // The injection branch is skipped here; scaling must still happen.
        let output = transformed_with(
            &[b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":800,\"output_tokens\":40}}}\n\n"],
            99,
            quarter_scale(),
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""input_tokens":200"#), "{text}");
        assert!(text.contains(r#""output_tokens":10"#), "{text}");
    }

    #[test]
    fn scaling_covers_every_field_the_compact_gate_sums() {
        let output = transformed_with(
            &[br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":400,"output_tokens":80,"cache_read_input_tokens":8000,"cache_creation_input_tokens":40,"server_tool_use":{"web_search_requests":3}}}

"#],
            99,
            quarter_scale(),
        );
        let text = String::from_utf8(output).unwrap();
        for expected in [
            r#""input_tokens":100"#,
            r#""output_tokens":20"#,
            r#""cache_read_input_tokens":2000"#,
            r#""cache_creation_input_tokens":10"#,
        ] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
        // A request count, not tokens.
        assert!(text.contains(r#""web_search_requests":3"#), "{text}");
    }

    #[test]
    fn partial_usage_shapes_keep_absent_fields_absent() {
        let output = transformed_with(
            &[b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":80}}\n\n"],
            99,
            quarter_scale(),
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r#""output_tokens":20"#), "{text}");
        assert!(!text.contains("cache_read_input_tokens"), "{text}");
        assert!(!text.contains("input_tokens\":"), "{text}");
    }

    #[test]
    fn small_windows_scale_up_and_round_half_away_from_zero() {
        // Real 125K into a believed 250K: report double.
        let scale = UsageScale::new(250_000, 125_000).unwrap();
        assert_eq!(scale.apply(7), 14);
        // 1/3 of a token rounds to the nearest whole one.
        let third = UsageScale::new(1, 3).unwrap();
        assert_eq!(third.apply(1), 0);
        assert_eq!(third.apply(2), 1);
        assert_eq!(third.apply(5), 2);
        assert!(UsageScale::new(1, 0).is_none());
    }

    #[test]
    fn without_a_scale_events_are_byte_identical() {
        let delta = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":42,\"output_tokens\":3}}\n\n";
        assert_eq!(transformed(&[delta], 99), delta);
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
