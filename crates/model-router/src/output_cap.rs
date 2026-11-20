//! Where to set `max-output-tokens`, appended to an open-weights host's
//! refusal of a request's output cap.
//!
//! Claude Code asks unrecognized models for 32000 output tokens (128000 to
//! continue a cut reply) and, since 2.1.329, no longer retries at a limit the
//! refusal names, so a host with a lower limit refuses those requests until
//! the route's `max-output-tokens` is set. Its bare 400 doesn't say so. No
//! number is parsed and nothing is retried; any other error passes through.

use axum::body::Bytes;
use serde_json::Value;

/// The sentence appended to a recognized refusal on the provider model
/// `routing_id`.
pub(crate) fn hint(routing_id: &str) -> String {
    format!(
        "model-router: set `max-output-tokens` to the host's limit on the \
         `routing-id = \"{routing_id}\"` model in config.toml, then run \
         `model-router service restart`"
    )
}

/// `body` with `hint` appended to its error message when it is an Anthropic
/// error envelope refusing the output cap; `None` passes the original
/// through.
pub(crate) fn with_hint(body: &[u8], hint: &str) -> Option<Bytes> {
    let mut envelope = serde_json::from_slice::<Value>(body).ok()?;
    let message = crate::overflow::invalid_request_message(&envelope)
        .filter(|message| refuses_output_cap(message))?;
    envelope["error"]["message"] = format!("{message} ({hint})").into();
    serde_json::to_vec(&envelope).ok().map(Bytes::from)
}

/// Whether `message` names the output-cap parameter together with an upper
/// bound on it. A shared-context refusal ("maximum context length") is
/// left alone: the room left depends on the prompt, which this setting
/// doesn't fix.
fn refuses_output_cap(message: &str) -> bool {
    const PARAMETERS: [&str; 2] = ["max_tokens", "max_completion_tokens"];
    const BOUNDS: [&str; 4] = ["valid range", "too large", "at most", "less than or equal"];
    let message = message.to_ascii_lowercase();
    PARAMETERS.iter().any(|name| message.contains(name))
        && BOUNDS.iter().any(|bound| message.contains(bound))
        && !message.contains("context")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DeepSeek`'s wording, as `CLIProxyAPI` 8.0.72 passed it on from a fake
    /// host (streamed and not, the same 400 JSON body).
    const CAPTURED_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Invalid max_tokens value, the valid range of max_tokens is [1, 4096]"}}"#;

    fn envelope(kind: &str, message: &str) -> String {
        serde_json::json!({"type": "error", "error": {"type": kind, "message": message}})
            .to_string()
    }

    fn message(body: &Bytes) -> String {
        let value: Value = serde_json::from_slice(body).unwrap();
        value["error"]["message"].as_str().unwrap().to_owned()
    }

    #[test]
    fn an_output_cap_refusal_gets_the_hint() {
        let hint = hint("deepseek-v4");
        let rewritten = with_hint(CAPTURED_BODY.as_bytes(), &hint).unwrap();
        assert_eq!(
            message(&rewritten),
            format!(
                "Invalid max_tokens value, the valid range of max_tokens is [1, 4096] ({hint})"
            )
        );
        assert!(hint.contains("routing-id = \"deepseek-v4\""));
        // Illustrative wordings of other hosts (not captured).
        for refusal in [
            "max_tokens is too large: 32000. This model supports at most 16384 completion tokens, \
             whereas you provided 32000.",
            "`max_tokens` must be less than or equal to `8192`",
            "max_completion_tokens: must be at most 8192",
        ] {
            let body = envelope("invalid_request_error", refusal);
            assert!(with_hint(body.as_bytes(), &hint).is_some(), "{refusal}");
        }
    }

    #[test]
    fn anything_else_passes_through() {
        let hint = hint("deepseek-v4");
        for message in [
            "Unsupported parameter: 'max_tokens'",
            "max_tokens must be an integer",
            "'max_tokens' or 'max_completion_tokens' is too large: 32000. This model's maximum \
             context length is 32768 tokens and your request has 1000 input tokens \
             (32000 > 32768 - 1000).",
            "This model's maximum context length is 131072 tokens.",
        ] {
            let body = envelope("invalid_request_error", message);
            assert!(with_hint(body.as_bytes(), &hint).is_none(), "{message}");
        }
        let other_type = envelope("api_error", "the valid range of max_tokens is [1, 4096]");
        assert!(with_hint(other_type.as_bytes(), &hint).is_none());
        assert!(with_hint(b"Bad Request", &hint).is_none());
    }
}
