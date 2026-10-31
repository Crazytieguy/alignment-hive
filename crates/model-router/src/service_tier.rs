//! Codex's priority ("fast") tier on the GPT branch, decided by the route
//! alone.
//!
//! A `-fast` twin ([`crate::config::ModelRoute::fast_tier`]) sends `service_tier:
//! "priority"`, the field and value native Codex sends for its `/fast`.
//! `CLIProxyAPI` (8.0.34) keeps it, and on a subscription login adds the
//! `X-Codex-Routing-Hint: model=…;tier=priority` header Codex sends. Every
//! other GPT-branch request loses any `speed` or `service_tier` it carries:
//! the child would map either to priority, xAI and Meta included, so the
//! tier is spent only when the model ID asks for it.
//!
//! Measured on GPT-6.1 Sol (2026-10-29 and 30, Pro plan):
//! - On hive recall's request (286K tokens, prefix cached), warm and
//!   interleaved, priority cut the median from 26.0 s to 16.0 s (n = 13 per
//!   tier, p ≈ 0.002), and no priority call took over 30 s against 3 of 13
//!   standard ones. That request runs at effort medium upstream: Claude
//!   Code's `$.model.complete` sends `output_config.effort` with no
//!   `thinking` block, which the child's translator ignores. At adaptive
//!   thinking with effort low, a shape recall never sends, the gain was small
//!   (12.8 s to 10.2 s, n = 3).
//! - Priority cut the median from 15.3 s to 7.6 s on uncached 17K-token
//!   requests through the router at effort low (adaptive thinking; n = 6
//!   each, interleaved), and from 12.55 s to 8.24 s on short direct ones.
//!   Sol's output is held until it is complete
//!   (`x-codex-safety-buffering-enabled`), so the first token gains nothing
//!   over the whole reply.
//! - The tiers share no prompt cache: after two calls warmed a prefix on
//!   one tier, the first call on the other read 0 cached tokens, in both
//!   orders.
//! - Every response reports `service_tier: "default"`, priority or not, so
//!   whether the tier applied is never observable; latency is the only sign.
//!
//! `OpenAI` bills the tier at 2.5x the included subscription usage (2x
//! purchased credits), and at about twice the per-token rate on an API key.

use serde_json::Value;

/// The tier fields a request may carry, which only the route decides.
const TIER_FIELDS: [&str; 2] = ["speed", "service_tier"];

/// Removes the top-level `speed` and `service_tier` from `document`, then
/// asks for the priority tier when `fast` is set.
pub fn apply(document: &mut Value, fast: bool) {
    let Some(object) = document.as_object_mut() else {
        return;
    };
    let removed: Vec<&str> = TIER_FIELDS
        .into_iter()
        .filter(|field| object.remove(*field).is_some())
        .collect();
    if fast {
        object.insert("service_tier".to_string(), Value::from("priority"));
    } else if !removed.is_empty() {
        tracing::debug!(fields = ?removed, "dropped a service tier the route does not ask for");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_twin_asks_for_priority_whatever_the_request_carried() {
        for inbound in [
            json!({"model": "m"}),
            json!({"model": "m", "speed": "fast"}),
            json!({"model": "m", "service_tier": "default", "speed": "normal"}),
        ] {
            let mut document = inbound;
            apply(&mut document, true);
            assert_eq!(document, json!({"model": "m", "service_tier": "priority"}));
        }
    }

    #[test]
    fn every_other_route_drops_the_tier_fields() {
        let mut document = json!({
            "model": "m", "speed": "fast", "service_tier": "priority",
            "metadata": {"speed": "fast"},
        });
        apply(&mut document, false);
        assert_eq!(
            document,
            json!({"model": "m", "metadata": {"speed": "fast"}})
        );
    }

    #[test]
    fn a_body_that_is_not_an_object_is_left_alone() {
        let mut document = json!(["speed"]);
        apply(&mut document, true);
        assert_eq!(document, json!(["speed"]));
    }
}
