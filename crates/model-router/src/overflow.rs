//! Translation of the Codex backend's context-overflow error into
//! Anthropic's canonical `prompt is too long` form.
//!
//! Claude Code recovers from context overflow — reactive compaction, then a
//! retry of the same round trip — only when the failure matches the
//! Anthropic API's public error surface: an `invalid_request_error` whose
//! message reads `prompt is too long: N tokens > M maximum`. The Codex
//! backend signals the same condition as `context_length_exceeded`, but
//! `CLIProxyAPI` drops that code in translation and forwards only the
//! message ("Your input exceeds the context window of this model. ..."),
//! which Claude Code classifies as a generic invalid request: no recovery
//! runs, and sessions retry the identical oversized request until a human
//! intervenes. Rewriting the message restores the recovery loop for every
//! Anthropic-protocol client, without depending on how any one client
//! implements it.
//!
//! Both wire shapes and the client behavior are recorded in
//! `plugins/model-router/docs/experiments.md` (measured against Claude Code
//! 2.1.220 and `CLIProxyAPI` 7.2.92; a version change on either side is a
//! retest trigger).

use bytes::Bytes;
use serde_json::Value;

/// Which backend's overflow dialect a route speaks.
///
/// Each variant owns exactly one verified phrase, and a rewrite only ever
/// tests its own. Matching the union would let a Codex route classify the
/// xAI phrase (and vice versa) as overflow — reintroducing, across
/// backends, the false positive the narrow gating exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverflowDialect {
    /// Codex backend, verified against `CLIProxyAPI` 7.2.92 (2026-07-29).
    Codex,
    /// xAI backend, verified against `CLIProxyAPI` 7.2.110 (2026-07-31):
    /// `This model's maximum prompt length is 500000 but the request
    /// contains 620215 tokens.`
    Xai,
}

impl OverflowDialect {
    /// The phrase identifying this backend's overflow message, compared with
    /// normalized whitespace and case so a rewording of the surrounding
    /// sentences or an embedded newline (observed in Codex's own test
    /// fixtures) does not break detection.
    ///
    /// Deliberately subject-bearing: a hypothetical "`max_tokens` exceeds
    /// the context window" must NOT match, because compaction cannot fix an
    /// output-limit error and a false positive would re-create the very
    /// retry loop this module removes.
    const fn phrase(self) -> &'static str {
        match self {
            Self::Codex => "input exceeds the context window",
            Self::Xai => "maximum prompt length is",
        }
    }
}

/// The `N` for one request, in whichever form is cheaper to carry.
///
/// Streaming requests already paid for a tiktoken estimate (it seeds
/// `message_start` usage), so they carry the number. Non-streaming requests
/// skip estimation on the happy path and carry the body instead — a
/// refcount, not a copy — to tokenize only if an overflow actually arrives.
/// The distinction also bounds memory: a streamed response can stay open
/// for a long generation, and holding every in-flight request's full body
/// for that long would pin arbitrarily large payloads.
#[derive(Clone, Debug)]
pub(crate) enum Estimate {
    Computed(u64),
    Deferred(Bytes),
}

/// Everything needed to rewrite one request's overflow error. Built only
/// for routes whose backend dialect is verified (see [`OverflowDialect`])
/// and whose real window is known.
#[derive(Clone, Debug)]
pub(crate) struct OverflowRewrite {
    /// The route's real context window: the `M` in `N tokens > M maximum`.
    window: u64,
    /// The request's estimated input tokens.
    estimate: Estimate,
    /// The only dialect this rewrite will recognize.
    dialect: OverflowDialect,
}

impl OverflowRewrite {
    pub(crate) const fn new(window: u64, estimate: Estimate, dialect: OverflowDialect) -> Self {
        Self {
            window,
            estimate,
            dialect,
        }
    }

    /// The canonical Anthropic message. `N` is the request's estimated
    /// input-token count, computed here when the request path deferred it,
    /// and clamped above the window: the estimator ignores non-text content
    /// (images, documents), and a mathematically false `N <= M` would deny
    /// the client's gap-guided compaction a positive overage. A minimal
    /// clamped gap only degrades recovery to one-group-at-a-time
    /// compaction, which still converges.
    fn message(&self) -> String {
        let estimate = match &self.estimate {
            Estimate::Computed(tokens) => *tokens,
            Estimate::Deferred(body) => crate::usage::estimate_input_tokens(body),
        };
        let n = estimate.max(self.window + 1);
        format!("prompt is too long: {n} tokens > {} maximum", self.window)
    }

    /// Rewrites an Anthropic error envelope (`{"type":"error","error":{..}}`)
    /// in place when it carries the Codex overflow error. Returns whether
    /// anything changed.
    pub(crate) fn rewrite_envelope(&self, envelope: &mut Value) -> bool {
        if envelope.get("type").and_then(Value::as_str) != Some("error") {
            return false;
        }
        let Some(error) = envelope.get_mut("error") else {
            return false;
        };
        if error.get("type").and_then(Value::as_str) != Some("invalid_request_error") {
            return false;
        }
        let Some(message) = error.get("message").and_then(Value::as_str) else {
            return false;
        };
        if !is_overflow_message(message, self.dialect) {
            return false;
        }
        error["message"] = Value::from(self.message());
        true
    }

    /// Rewrites a buffered HTTP error body when it carries the Codex
    /// overflow error. `None` means pass the original bytes through.
    pub(crate) fn rewrite_body(&self, body: &[u8]) -> Option<Bytes> {
        let mut envelope = serde_json::from_slice::<Value>(body).ok()?;
        if !self.rewrite_envelope(&mut envelope) {
            return None;
        }
        serde_json::to_vec(&envelope).ok().map(Bytes::from)
    }
}

fn is_overflow_message(message: &str, dialect: OverflowDialect) -> bool {
    let normalized = message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    normalized.contains(dialect.phrase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact body captured live from `CLIProxyAPI` 7.2.92 (2026-07-29).
    const CAPTURED_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your input exceeds the context window of this model. Please adjust your input and try again."}}"#;

    fn rewrite(estimate: u64) -> OverflowRewrite {
        OverflowRewrite::new(
            258_400,
            Estimate::Computed(estimate),
            OverflowDialect::Codex,
        )
    }

    fn message_of(body: &Bytes) -> String {
        let envelope = serde_json::from_slice::<Value>(body).unwrap();
        envelope["error"]["message"].as_str().unwrap().to_string()
    }

    #[test]
    fn captured_overflow_body_is_rewritten_to_the_canonical_form() {
        let rewritten = rewrite(300_000)
            .rewrite_body(CAPTURED_BODY.as_bytes())
            .unwrap();
        assert_eq!(
            message_of(&rewritten),
            "prompt is too long: 300000 tokens > 258400 maximum"
        );
        let envelope = serde_json::from_slice::<Value>(&rewritten).unwrap();
        assert_eq!(envelope["type"], "error");
        assert_eq!(envelope["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn detection_survives_embedded_newlines_and_case_changes() {
        // Codex's own test fixtures include a message with an embedded
        // newline; whitespace and case are normalized before matching.
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your INPUT exceeds\nthe   context\twindow of this model. Please try\nagain."}}"#;
        assert!(rewrite(300_000).rewrite_body(body.as_bytes()).is_some());
    }

    #[test]
    fn an_underestimate_is_clamped_above_the_window() {
        // Estimator undercounts (e.g. image-heavy request): never emit
        // `N <= M`.
        let rewritten = rewrite(100).rewrite_body(CAPTURED_BODY.as_bytes()).unwrap();
        assert_eq!(
            message_of(&rewritten),
            "prompt is too long: 258401 tokens > 258400 maximum"
        );
    }

    #[test]
    fn a_deferred_estimate_is_computed_from_the_retained_request_body() {
        // Non-streaming requests skip estimation and retain the body
        // instead; the rewrite tokenizes it lazily. This request body
        // estimates well past the window, so N is real, not the clamp.
        let request = format!(
            r#"{{"model":"gpt-test","messages":[{{"role":"user","content":"{}"}}]}}"#,
            "alpha bravo charlie delta echo ".repeat(60_000)
        );
        let rewritten = OverflowRewrite::new(
            258_400,
            Estimate::Deferred(Bytes::from(request)),
            OverflowDialect::Codex,
        )
        .rewrite_body(CAPTURED_BODY.as_bytes())
        .unwrap();
        let message = message_of(&rewritten);
        let n: u64 = message
            .strip_prefix("prompt is too long: ")
            .and_then(|rest| rest.split(' ').next())
            .unwrap()
            .parse()
            .unwrap();
        assert!(n > 258_401, "expected a real estimate, got {message}");
        assert!(message.ends_with("tokens > 258400 maximum"), "{message}");
    }

    #[test]
    fn output_limit_and_unrelated_errors_pass_through() {
        for body in [
            // Subject-bearing match required: max_tokens phrasing must not
            // trigger compaction.
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"requested max_tokens exceeds the context window of this model"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"model output exceeds the context window"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"some other validation failure"}}"#,
            // Right message, wrong error type.
            r#"{"type":"error","error":{"type":"api_error","message":"Your input exceeds the context window of this model."}}"#,
            // Not an error envelope.
            r#"{"type":"message","content":[]}"#,
            // Not JSON.
            "input exceeds the context window",
        ] {
            assert!(
                rewrite(300_000).rewrite_body(body.as_bytes()).is_none(),
                "must pass through: {body}"
            );
        }
    }

    /// The exact body captured live from `CLIProxyAPI` 7.2.110 against xAI
    /// (2026-07-31): 697K tokens sent to grok-4.5's 500K window.
    const CAPTURED_XAI_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"{\"code\":\"invalid-argument\",\"error\":\"This model's maximum prompt length is 500000 but the request contains 620215 tokens.\"}"}}"#;

    #[test]
    fn captured_xai_overflow_body_is_translated() {
        let rewrite =
            OverflowRewrite::new(500_000, Estimate::Computed(620_215), OverflowDialect::Xai);
        let rewritten = rewrite
            .rewrite_body(&Bytes::from(CAPTURED_XAI_BODY))
            .expect("xai overflow body is rewritten");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"],
            "prompt is too long: 620215 tokens > 500000 maximum"
        );
    }

    #[test]
    fn an_output_limit_error_is_never_mistaken_for_overflow() {
        // Compaction cannot fix an output-limit error, so these must not
        // match either dialect's phrase.
        for message in [
            "max_tokens exceeds the model's output limit",
            "This model's maximum output length is 65536",
            "requested max_tokens is too large",
        ] {
            for dialect in [OverflowDialect::Codex, OverflowDialect::Xai] {
                assert!(
                    !is_overflow_message(message, dialect),
                    "{message} / {dialect:?}"
                );
            }
        }
    }

    #[test]
    fn a_rewrite_never_matches_the_other_backends_dialect() {
        // The whole point of the dialect: a Codex route seeing xAI's wording
        // (or vice versa) must NOT classify it as overflow, or the client
        // compacts and retries a request that was never too long.
        let codex =
            OverflowRewrite::new(258_400, Estimate::Computed(300_000), OverflowDialect::Codex);
        let xai = OverflowRewrite::new(500_000, Estimate::Computed(620_215), OverflowDialect::Xai);

        // Each accepts its own captured body ...
        assert!(codex.rewrite_body(&Bytes::from(CAPTURED_BODY)).is_some());
        assert!(xai.rewrite_body(&Bytes::from(CAPTURED_XAI_BODY)).is_some());

        // ... and rejects the other's.
        assert!(
            codex
                .rewrite_body(&Bytes::from(CAPTURED_XAI_BODY))
                .is_none(),
            "Codex rewrite must not claim the xAI overflow body"
        );
        assert!(
            xai.rewrite_body(&Bytes::from(CAPTURED_BODY)).is_none(),
            "xAI rewrite must not claim the Codex overflow body"
        );
    }

    #[test]
    fn dialect_phrases_are_disjoint_on_the_captured_bodies() {
        for (dialect, own, other) in [
            (OverflowDialect::Codex, CAPTURED_BODY, CAPTURED_XAI_BODY),
            (OverflowDialect::Xai, CAPTURED_XAI_BODY, CAPTURED_BODY),
        ] {
            let message = |body: &str| -> String {
                serde_json::from_str::<Value>(body).unwrap()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            assert!(is_overflow_message(&message(own), dialect), "{dialect:?}");
            assert!(
                !is_overflow_message(&message(other), dialect),
                "{dialect:?}"
            );
        }
    }
}
