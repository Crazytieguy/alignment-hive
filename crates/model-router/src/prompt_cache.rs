//! Shared-prefix prompt-cache identity for GPT-routed requests.
//!
//! `CLIProxyAPI` derives the upstream Codex `prompt_cache_key` (and the
//! `Session_id` header) from the Claude Code session and agent headers, and
//! `OpenAI` routes requests to cache nodes by that key — so under the
//! per-conversation identities those headers normally carry, byte-identical
//! prefixes in different conversations never share the upstream prompt
//! cache: every new session and subagent spawn paid its full prompt as a
//! cache miss on its first request. The GPT egress paths instead rewrite
//! those headers to [`shared_prefix_key`], a stable hash of the forwarded
//! body's model and system-prompt head, so conversations of the same prefix
//! family — same agent type, same project — reuse each other's cache.
//!
//! Accepted trade-off: `CLIProxyAPI` scopes its reasoning-replay fallback
//! store by these same headers (there is no way to decouple the two from
//! outside), so one bounded store is shared per prefix family. Claude Code
//! echoes reasoning signatures itself — the store's primary path defers to
//! them — which keeps the blast radius to the store's rarely-hit fallback
//! cases.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// How many system-prompt bytes participate in the shared-prefix identity.
///
/// The hash runs over the forwarded body, whose system prompt leads with the
/// router's constant identity block ([`crate::identity`]), so the budget
/// leaves room for the discriminating content behind it: agent bodies
/// diverge within their first line, while the volatile tail of Claude
/// Code's system prompts (git status, memory index) sits well past it.
const HEAD_BUDGET_BYTES: usize = 2048;

/// The per-conversation Claude Code attribution block. Volatile per
/// conversation (it carries a content hash), and `CLIProxyAPI` strips it
/// before the Codex upstream sees the prompt, so it must not participate in
/// the prefix identity either.
const ATTRIBUTION_PREFIX: &str = "x-anthropic-billing-header:";

/// Derives the shared-prefix prompt-cache identity for a GPT-bound request:
/// a stable function of the body's model and system-prompt head. Computed at
/// the egress layer over the forwarded body — itself a deterministic
/// function of the client body and route — so every GPT forwarding path
/// carries it structurally.
///
/// Reads only the top-level `model` and `system` values (DOM-free scan, like
/// [`crate::routing::substitute_model`]); the conversation is never parsed.
///
/// Returns `None` when the body cannot be scanned or `system` has an
/// unsupported shape; the caller forwards the request unmodified.
#[must_use]
pub fn shared_prefix_key(body: &[u8]) -> Option<String> {
    let model_range = crate::routing::find_top_level_value_range(body, "model")?;
    let model: String = serde_json::from_slice(&body[model_range]).ok()?;

    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update([0]);

    if let Some(system_range) = crate::routing::find_top_level_value_range(body, "system") {
        let system: Value = serde_json::from_slice(&body[system_range]).ok()?;
        let mut remaining = HEAD_BUDGET_BYTES;
        match &system {
            Value::String(text) => hash_within_budget(&mut hasher, text, &mut remaining),
            Value::Array(blocks) => {
                for block in blocks {
                    let Some(text) = block.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    if text.trim_start().starts_with(ATTRIBUTION_PREFIX) {
                        continue;
                    }
                    hash_within_budget(&mut hasher, text, &mut remaining);
                    if remaining == 0 {
                        break;
                    }
                }
            }
            _ => return None,
        }
    }

    let digest = hasher.finalize();
    Some(format!(
        "prefix-{}",
        crate::state::hex_encode(&digest[..16])
    ))
}

fn hash_within_budget(hasher: &mut Sha256, text: &str, remaining: &mut usize) {
    let slice = truncate_to(text, *remaining);
    hasher.update(slice.as_bytes());
    *remaining -= slice.len();
}

/// The longest prefix of `text` that fits in `budget` bytes without
/// splitting a UTF-8 character.
fn truncate_to(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut end = budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(model: &str, system: &str) -> Vec<u8> {
        format!(r#"{{"model":"{model}","system":{system},"messages":[]}}"#).into_bytes()
    }

    #[test]
    fn identical_heads_share_a_key_across_differing_tails() {
        let long_head = "s".repeat(HEAD_BUDGET_BYTES);
        let a = body("gpt-5.6-luna", &format!(r#""{long_head} tail one""#));
        let b = body(
            "gpt-5.6-luna",
            &format!(r#""{long_head} another tail entirely""#),
        );
        assert_eq!(shared_prefix_key(&a), shared_prefix_key(&b));
    }

    #[test]
    fn early_divergence_and_model_both_change_the_key() {
        let a = shared_prefix_key(&body("gpt-5.6-luna", r#""agent one prompt""#)).unwrap();
        let b = shared_prefix_key(&body("gpt-5.6-luna", r#""agent two prompt""#)).unwrap();
        let c = shared_prefix_key(&body("gpt-5.6-sol", r#""agent one prompt""#)).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn attribution_blocks_do_not_participate() {
        let with = body(
            "gpt-5.6-luna",
            r#"[{"type":"text","text":"x-anthropic-billing-header: cc_version=1; cch=aaaa;"},{"type":"text","text":"You are an agent."}]"#,
        );
        let without = body(
            "gpt-5.6-luna",
            r#"[{"type":"text","text":"You are an agent."}]"#,
        );
        assert_eq!(shared_prefix_key(&with), shared_prefix_key(&without));
        let other_conversation = body(
            "gpt-5.6-luna",
            r#"[{"type":"text","text":"x-anthropic-billing-header: cc_version=1; cch=bbbb;"},{"type":"text","text":"You are an agent."}]"#,
        );
        assert_eq!(
            shared_prefix_key(&with),
            shared_prefix_key(&other_conversation)
        );
    }

    #[test]
    fn a_leading_constant_block_leaves_budget_for_the_discriminating_one() {
        let identity = crate::identity::identity_text("GPT Test").replace('"', "");
        let a = body(
            "gpt-5.6-luna",
            &format!(
                r#"[{{"type":"text","text":"{identity}"}},{{"type":"text","text":"agent one"}}]"#
            ),
        );
        let b = body(
            "gpt-5.6-luna",
            &format!(
                r#"[{{"type":"text","text":"{identity}"}},{{"type":"text","text":"agent two"}}]"#
            ),
        );
        assert_ne!(shared_prefix_key(&a), shared_prefix_key(&b));
    }

    #[test]
    fn head_spans_blocks_and_respects_char_boundaries() {
        let filler = "é".repeat(HEAD_BUDGET_BYTES / 2); // 2-byte chars straddle the budget
        let a = body(
            "gpt-5.6-luna",
            &format!(
                r#"[{{"type":"text","text":"first block"}},{{"type":"text","text":"{filler}A"}}]"#
            ),
        );
        let b = body(
            "gpt-5.6-luna",
            &format!(
                r#"[{{"type":"text","text":"first block"}},{{"type":"text","text":"{filler}B"}}]"#
            ),
        );
        // Both truncate inside the filler: same key despite differing tails.
        assert_eq!(shared_prefix_key(&a), shared_prefix_key(&b));
    }

    #[test]
    fn missing_system_still_derives_a_per_model_key() {
        let luna = br#"{"model":"gpt-5.6-luna","messages":[]}"#;
        let sol = br#"{"model":"gpt-5.6-sol","messages":[]}"#;
        assert!(shared_prefix_key(luna).is_some());
        assert_ne!(shared_prefix_key(luna), shared_prefix_key(sol));
    }

    #[test]
    fn unsupported_shapes_yield_none() {
        assert!(shared_prefix_key(b"not json").is_none());
        assert!(shared_prefix_key(&body("m", "42")).is_none());
        assert!(shared_prefix_key(br#"{"system":"x","messages":[]}"#).is_none());
    }
}
