//! Translation of a routed backend's context-overflow error into
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
//! The xAI backend has its own phrase; each backend is a dialect
//! ([`OverflowDialect`]) and a route only ever matches its own. Meta's
//! phrase is generic, so the Meta dialect also needs the request's own
//! estimate to be near the window ([`OverflowDialect::min_estimate_percent`]).
//!
//! The Codex and xAI wire shapes and the client behavior are recorded in
//! `experiments/model-router.md` (measured against Claude Code
//! 2.1.220 and `CLIProxyAPI` 7.2.92; a version change on either side is a
//! retest trigger).

use bytes::Bytes;
use serde_json::Value;

/// Which backend's overflow dialect a route speaks.
///
/// Each variant owns its own verified phrases, and a rewrite only ever
/// tests its own. Matching the union would let a Codex route classify the
/// xAI phrase (and vice versa) as overflow — reintroducing, across
/// backends, the false positive the narrow gating exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverflowDialect {
    /// Codex backend, verified against `CLIProxyAPI` 7.2.92 (2026-07-29).
    Codex,
    /// xAI backend, verified against `CLIProxyAPI` 7.2.110 (2026-07-31):
    /// `This model's maximum prompt length is 500000 but the request
    /// contains 620215 tokens.` Reworded upstream by 2026-09-23 (every Grok
    /// route, `CLIProxyAPI` 7.3.16): `Failed to start sampling:
    /// [input_too_large] The prompt is too long for this model's context
    /// window (541372 tokens > 500000 tokens)`. Both are accepted.
    Xai,
    /// Meta backend, captured against `CLIProxyAPI` 8.0.30 (2026-10-15):
    /// every over-window request is a 400 `invalid_request_error` reading
    /// `The request contains invalid parameters. Check the request body for
    /// any errors or inconsistencies.` — no context wording, and the same
    /// text Meta uses for other invalid parameters. Matched only together
    /// with [`META_MIN_ESTIMATE_PERCENT`].
    Meta,
}

/// How near the window a Muse request's own estimate must be before Meta's
/// generic invalid-parameters error is read as overflow, in percent of the
/// route's window (1,007,997 on the built-in Muse routes, so 806,398).
///
/// Measured 2026-10-15 on `muse-wonder-1.0`: 999,107 Meta tokens succeeded
/// and ~1,060,000 failed, so no request under 999,107 Meta tokens overflows.
/// The router's o200k estimate tracks Meta's count within ~1-2% on text:
/// o200k counts 0-2% more than Meta's tokenizer on prose and code, and Meta
/// adds a fixed ~881 formatting tokens for Claude Code's 12 tool definitions
/// (~0.1% at 1M). The margin is for what the estimate cannot see: images,
/// which it ignores, and scripts where Meta counts more (Hebrew ~8%, emoji
/// ~25%; 80% covers an undercount of up to ~19%). An unrelated invalid
/// parameter on any request estimated under 806,398 tokens (~190K below the
/// largest request Meta accepted) passes through untouched.
///
/// Leaning low is the cheaper mistake. A false positive (a large request
/// that really had another invalid parameter) costs one compaction, after
/// which the request is far under the threshold and the real error passes
/// through. A false negative leaves the session retrying the same oversized
/// request, the loop this module exists to break.
const META_MIN_ESTIMATE_PERCENT: u64 = 80;

impl OverflowDialect {
    /// The phrases identifying this backend's overflow message, compared with
    /// normalized whitespace and case so a rewording of the surrounding
    /// sentences or an embedded newline (observed in Codex's own test
    /// fixtures) does not break detection.
    ///
    /// Deliberately subject-bearing: a hypothetical "`max_tokens` exceeds
    /// the context window" must NOT match, because compaction cannot fix an
    /// output-limit error and a false positive would re-create the very
    /// retry loop this module removes. Meta's phrase has no subject to bear,
    /// which is why it also needs [`Self::min_estimate_percent`].
    const fn phrases(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["input exceeds the context window"],
            Self::Xai => &[
                "maximum prompt length is",
                "prompt is too long for this model's context window",
            ],
            Self::Meta => &["the request contains invalid parameters"],
        }
    }

    /// For a dialect whose phrase does not by itself mean overflow, the
    /// share of the window, in percent, the request's own estimate must
    /// reach before the phrase is read as overflow. `None` when the phrase
    /// alone is proof.
    const fn min_estimate_percent(self) -> Option<u64> {
        match self {
            Self::Codex | Self::Xai => None,
            Self::Meta => Some(META_MIN_ESTIMATE_PERCENT),
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

    /// The request's estimated input-token count, computed here when the
    /// request path deferred it.
    fn estimate(&self) -> u64 {
        match &self.estimate {
            Estimate::Computed(tokens) => *tokens,
            Estimate::Deferred(body) => crate::usage::estimate_input_tokens(body),
        }
    }

    /// Whether the estimate is near enough the window for this dialect's
    /// phrase to mean overflow.
    fn near_window(&self, estimate: u64) -> bool {
        self.dialect.min_estimate_percent().is_none_or(|percent| {
            u128::from(estimate) * 100 >= u128::from(self.window) * u128::from(percent)
        })
    }

    /// The canonical Anthropic message. `N` is the request's estimate,
    /// clamped above the window: the estimator ignores non-text content
    /// (images, documents), and a mathematically false `N <= M` would deny
    /// the client's gap-guided compaction a positive overage. A minimal
    /// clamped gap only degrades recovery to one-group-at-a-time
    /// compaction, which still converges.
    fn message(&self, estimate: u64) -> String {
        let n = estimate.max(self.window + 1);
        format!("prompt is too long: {n} tokens > {} maximum", self.window)
    }

    /// Rewrites an Anthropic error envelope (`{"type":"error","error":{..}}`)
    /// in place when it carries the route's backend overflow error. Returns
    /// whether anything changed.
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
        let estimate = self.estimate();
        if !self.near_window(estimate) {
            return false;
        }
        error["message"] = Value::from(self.message(estimate));
        true
    }

    /// Rewrites a buffered HTTP error body when it carries the route's
    /// backend overflow error. `None` means pass the original bytes through.
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
    dialect
        .phrases()
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The exact body captured live from `CLIProxyAPI` 7.2.92 (2026-07-29).
    pub(crate) const CAPTURED_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your input exceeds the context window of this model. Please adjust your input and try again."}}"#;

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
            // Subject-bearing match required: compaction cannot fix an
            // output-limit error, so max_tokens phrasing must not match
            // either dialect.
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"requested max_tokens exceeds the context window of this model"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"model output exceeds the context window"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"This model's maximum output length is 65536"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"some other validation failure"}}"#,
            // Right message, wrong error type.
            r#"{"type":"error","error":{"type":"api_error","message":"Your input exceeds the context window of this model."}}"#,
            // Not an error envelope.
            r#"{"type":"message","content":[]}"#,
            // Not JSON.
            "input exceeds the context window",
        ] {
            for dialect in [
                OverflowDialect::Codex,
                OverflowDialect::Xai,
                OverflowDialect::Meta,
            ] {
                let rewrite = OverflowRewrite::new(258_400, Estimate::Computed(300_000), dialect);
                assert!(
                    rewrite.rewrite_body(body.as_bytes()).is_none(),
                    "must pass through: {body} / {dialect:?}"
                );
            }
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

    /// The reworded xAI body captured live from `CLIProxyAPI` 7.3.16
    /// (2026-09-23): 541K tokens sent to grok-4.7's 500K window; grok-4.6
    /// answered the same way.
    const CAPTURED_XAI_BODY_2026_09: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"{\"code\":\"invalid-argument\",\"error\":\"Failed to start sampling: [input_too_large] The prompt is too long for this model's context window (541372 tokens \u003e 500000 tokens)\"}"}}"#;

    #[test]
    fn reworded_xai_overflow_body_is_translated() {
        let rewrite =
            OverflowRewrite::new(500_000, Estimate::Computed(541_372), OverflowDialect::Xai);
        let rewritten = rewrite
            .rewrite_body(&Bytes::from(CAPTURED_XAI_BODY_2026_09))
            .expect("reworded xai overflow body is rewritten");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            value["error"]["message"],
            "prompt is too long: 541372 tokens > 500000 maximum"
        );
        let codex =
            OverflowRewrite::new(258_400, Estimate::Computed(541_372), OverflowDialect::Codex);
        assert!(
            codex
                .rewrite_body(&Bytes::from(CAPTURED_XAI_BODY_2026_09))
                .is_none()
        );
    }

    #[test]
    fn a_rewrite_never_matches_the_other_backends_dialect() {
        // Every estimate is far past every window, so only the dialect
        // decides.
        let codex = OverflowRewrite::new(
            258_400,
            Estimate::Computed(2_000_000),
            OverflowDialect::Codex,
        );
        let xai =
            OverflowRewrite::new(500_000, Estimate::Computed(2_000_000), OverflowDialect::Xai);
        let meta = OverflowRewrite::new(
            MUSE_WINDOW,
            Estimate::Computed(2_000_000),
            OverflowDialect::Meta,
        );
        let bodies = [
            (OverflowDialect::Codex, CAPTURED_BODY),
            (OverflowDialect::Xai, CAPTURED_XAI_BODY),
            (OverflowDialect::Meta, CAPTURED_META_BODY),
        ];
        for rewrite in [codex, xai, meta] {
            for (dialect, body) in bodies {
                // Each accepts its own captured body and rejects the others'.
                assert_eq!(
                    rewrite.rewrite_body(&Bytes::from(body)).is_some(),
                    dialect == rewrite.dialect,
                    "{:?} rewrite on the {dialect:?} body",
                    rewrite.dialect
                );
            }
        }
    }

    /// The body `CLIProxyAPI` 8.0.30 delivered for ~1.06M Meta tokens sent
    /// to `muse-wonder-1.0` (2026-10-15). Meta's own body was
    /// `{"error":{"code":null,"message":"<same>","param":null,
    /// "type":"invalid_request_error"}}`.
    const CAPTURED_META_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"The request contains invalid parameters. Check the request body for any errors or inconsistencies."}}"#;

    /// The built-in Muse routes' window.
    const MUSE_WINDOW: u64 = 1_007_997;

    fn meta(estimate: Estimate) -> OverflowRewrite {
        OverflowRewrite::new(MUSE_WINDOW, estimate, OverflowDialect::Meta)
    }

    #[test]
    fn meta_generic_error_is_translated_only_near_the_window() {
        // The router's estimate of the request that failed live.
        let rewritten = meta(Estimate::Computed(1_059_908))
            .rewrite_body(CAPTURED_META_BODY.as_bytes())
            .expect("an over-window request's generic error is overflow");
        assert_eq!(
            message_of(&rewritten),
            "prompt is too long: 1059908 tokens > 1007997 maximum"
        );
        // Between the threshold and the window (the tokenizers may disagree),
        // N is clamped above the window.
        let rewritten = meta(Estimate::Computed(900_000))
            .rewrite_body(CAPTURED_META_BODY.as_bytes())
            .unwrap();
        assert_eq!(
            message_of(&rewritten),
            "prompt is too long: 1007998 tokens > 1007997 maximum"
        );
        // The threshold is 80% of the window, inclusive: 806,397.6.
        assert!(
            meta(Estimate::Computed(806_398))
                .rewrite_body(CAPTURED_META_BODY.as_bytes())
                .is_some()
        );
        // The same message on a smaller request is some other invalid
        // parameter, and reaches the client untouched.
        for small in [806_397, 9_008, 0] {
            assert!(
                meta(Estimate::Computed(small))
                    .rewrite_body(CAPTURED_META_BODY.as_bytes())
                    .is_none(),
                "{small}"
            );
        }
        // A deferred estimate is judged the same way.
        let small_request =
            br#"{"model":"muse-wonder-1.0","messages":[{"role":"user","content":"hello"}]}"#;
        assert!(
            meta(Estimate::Deferred(Bytes::from_static(small_request)))
                .rewrite_body(CAPTURED_META_BODY.as_bytes())
                .is_none()
        );
        // Codex and xAI need no size evidence: their phrases name overflow.
        for dialect in [OverflowDialect::Codex, OverflowDialect::Xai] {
            assert!(OverflowRewrite::new(500_000, Estimate::Computed(1), dialect).near_window(1));
        }
    }

    /// Random six-digit numbers, the text of the live Meta probes.
    fn dense_numbers(count: usize) -> String {
        let mut state: u64 = 11;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (100_000 + (state >> 33) % 900_000).to_string()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The tokenizer comparison behind [`META_MIN_ESTIMATE_PERCENT`]: Meta
    /// billed 9,104 input tokens for 3,003 such numbers plus a short
    /// instruction, and 999,107 for 333,003 (live, 2026-10-15). The router
    /// estimated those bodies at 9,008 and 999,008: exactly 3 per number (a
    /// space, then two 3-digit groups) as in Meta's count, with Meta ~100
    /// tokens higher overall. On this text the undercount is ~1% at worst,
    /// far inside the threshold's margin.
    #[test]
    fn the_estimator_counts_dense_numbers_as_meta_does() {
        let count = 3_003;
        let body = serde_json::json!({
            "model": "muse-wonder-1.0",
            "messages": [{"role": "user",
                          "content": format!("{}\n\nReply with: ok", dense_numbers(count))}],
        });
        let estimate = crate::usage::estimate_input_tokens(body.to_string().as_bytes());
        let numbers = 3 * count as u64;
        assert!(
            (numbers..numbers + 20).contains(&estimate),
            "estimate {estimate} for {count} numbers"
        );
        let meta_billed = 9_104_u64;
        assert!(
            estimate * 100 >= meta_billed * 98,
            "{estimate} vs {meta_billed}"
        );
    }
}
