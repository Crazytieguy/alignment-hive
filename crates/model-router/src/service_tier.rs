//! Codex's priority ("fast") tier on the GPT branch, decided by the route
//! alone.
//!
//! A `-fast` twin ([`crate::config::ModelRoute::tier`]) sends `service_tier:
//! "priority"`, the field and value native Codex sends for its `/fast`.
//! `CLIProxyAPI` (8.0.49) keeps it, and on a subscription login adds the
//! `X-Codex-Routing-Hint: model=…;tier=priority` header Codex sends. Every
//! other GPT-branch request loses any `speed` or `service_tier` it carries:
//! the child would map either to priority, xAI and Meta included, so the
//! tier is spent only when the model ID asks for it.
//!
//! Measured on GPT-6.1 Sol (2026-10-29 and 30, Pro plan):
//! - On a 256K–286K-token request with its prefix cached (hive recall's),
//!   warm and interleaved: at effort low, priority cut the mean from 10.9 s to 6.6 s
//!   (n = 20 each); at effort medium, the median from 26.0 s to 16.0 s
//!   (n = 13 each, p ≈ 0.002).
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
//!
//! An `-ultrafast` twin (managed mode) asks for `OpenAI`'s
//! Ultrafast tier, which only GPT-6.1 Sol and GPT-6 Astra have and only
//! some plans may use (Codex's speed page, 2026-11-01: Pro $500 and eligible
//! Enterprise and Edu, at 8x the included usage, 6x credits; every API key,
//! at API prices). "Up to 8x faster" than Standard is `OpenAI`'s claim for
//! both models in Codex (the speed page for Astra, the 2026-10-08 rollout
//! post for Sol); it is unmeasured here. The body cannot carry it: `CLIProxyAPI`'s Claude
//! translator keeps only `priority` (`normalizeCodexServiceTier`, 8.0.44
//! through 8.0.51). So the router sends the twin's turns with
//! [`ULTRAFAST_RELAY_HEADER`] instead, and the managed child's payload rule
//! (`supervisor::upstream_config_yaml`) writes `service_tier: "ultrafast"`
//! into the final upstream body of a request that carries it. That rule runs
//! in the Codex executor's `cacheHelper` (`helps.FinalizePayload`), on the
//! websocket path too, before the routing hint reads the tier from the same
//! body. The child reads the header from the request the router sent it
//! (`opts.Headers`, a clone of the inbound request's).
//!
//! Measured on the $200 Pro plan (2026-11-01), which has no Ultrafast:
//! `OpenAI` accepts the tier without an error and answers slower than at
//! priority (45–80 s against 26–28 s for ~800 output tokens, n = 3 each),
//! and the account's own Codex catalog lists only `priority`. So a
//! subscription's twin turns are refused unless that catalog lists the tier,
//! and the twins are offered only where it does ([`crate::ultrafast`]).

use serde_json::Value;

/// The router-private header that marks an `-ultrafast` twin's request to
/// the managed child. Only the router adds it, and every GPT-branch request
/// loses a client's copy (`headers::request_headers`).
pub const ULTRAFAST_RELAY_HEADER: &str = "x-model-router-service-tier";

/// [`ULTRAFAST_RELAY_HEADER`]'s value, and the tier the child writes for it.
pub const ULTRAFAST: &str = "ultrafast";

/// The tier fields a request may carry, which only the route decides.
const TIER_FIELDS: [&str; 2] = ["speed", "service_tier"];

/// The Codex service tier a route asks for ([`crate::config::ModelRoute::tier`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceTier {
    /// Whatever the upstream does by default: every route but a twin.
    #[default]
    Standard,
    /// Codex's priority tier (`/fast`): the `-fast` twins.
    Priority,
    /// `OpenAI`'s Ultrafast tier: the `-ultrafast` twins
    /// ([`ULTRAFAST_RELAY_HEADER`]).
    Ultrafast,
}

impl ServiceTier {
    /// The tier of a `WebSearch` sub-call or an auto-mode classifier call
    /// made for this route: the route's own, but Ultrafast's run at
    /// priority. Those calls go out without the relay; the person opted in to
    /// speed, and priority is the nearest tier they have.
    #[must_use]
    pub const fn side_call_tier(self) -> Self {
        match self {
            Self::Ultrafast => Self::Priority,
            tier => tier,
        }
    }
}

/// Removes the top-level `speed` and `service_tier` from `document`, then
/// asks for the priority tier when `tier` is [`ServiceTier::Priority`].
/// Ultrafast cannot ride the body ([`ULTRAFAST_RELAY_HEADER`]), so its
/// requests leave here with neither field.
pub fn apply(document: &mut Value, tier: ServiceTier) {
    let Some(object) = document.as_object_mut() else {
        return;
    };
    let removed: Vec<&str> = TIER_FIELDS
        .into_iter()
        .filter(|field| object.remove(*field).is_some())
        .collect();
    if tier == ServiceTier::Priority {
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
            apply(&mut document, ServiceTier::Priority);
            assert_eq!(document, json!({"model": "m", "service_tier": "priority"}));
        }
    }

    #[test]
    fn every_other_route_drops_the_tier_fields() {
        let mut document = json!({
            "model": "m", "speed": "fast", "service_tier": "priority",
            "metadata": {"speed": "fast"},
        });
        apply(&mut document, ServiceTier::Standard);
        assert_eq!(
            document,
            json!({"model": "m", "metadata": {"speed": "fast"}})
        );
    }

    #[test]
    fn a_body_that_is_not_an_object_is_left_alone() {
        let mut document = json!(["speed"]);
        apply(&mut document, ServiceTier::Priority);
        assert_eq!(document, json!(["speed"]));
    }
}
