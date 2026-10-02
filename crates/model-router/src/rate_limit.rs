//! Optional per-route upstream rate limiting.
//!
//! When configured, routed (non-Claude) requests acquire a concurrency permit
//! before reaching the upstream. The permit is held through the complete
//! response — including streaming — so a slot is occupied until the client has
//! received every byte, not just the headers. Requests that cannot acquire a
//! permit within the configured timeout receive a 429 in the Anthropic error
//! shape that Claude Code already handles.
//!
//! Resolution order: route-specific limit > family-specific limit > global
//! default. A route with no matching rule at any level is unlimited.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::{Config, ModelFamily, ModelRoute};

/// A resolved concurrency gate: one semaphore shared by every route that maps
/// to the same bucket.
#[derive(Clone)]
struct Gate {
    semaphore: Arc<Semaphore>,
    timeout: Duration,
}

/// The set of concurrency gates built from the config. Cheap to clone (all
/// `Arc`).
#[derive(Clone, Default)]
pub struct RateLimiter {
    /// Keyed by `routing_id`.
    per_route: HashMap<String, Gate>,
    /// Keyed by family label.
    per_family: HashMap<String, Gate>,
    /// Fallback for any routed request without a more specific gate.
    global: Option<Gate>,
}

/// A held permit. Dropping it returns the slot to the semaphore, so the
/// caller must keep it alive for the duration of the response.
pub struct Permit(#[allow(dead_code)] OwnedSemaphorePermit);

/// Why a permit could not be acquired.
#[derive(Debug)]
pub enum Denied {
    /// The queue timeout expired.
    TimedOut,
}

impl RateLimiter {
    /// Builds the limiter from the parsed config. Routes and families without
    /// an explicit rule are unlimited. Returns `None` when the config has no
    /// rate-limit section at all (the common case), so the hot path is a single
    /// `Option::is_none` check.
    #[must_use]
    pub fn from_config(config: &Config) -> Option<Self> {
        let rl = config.rate_limit.as_ref()?;

        let global = rl.max_concurrent.map(|max| Gate {
            semaphore: Arc::new(Semaphore::new(max)),
            timeout: rl.queue_timeout,
        });

        let per_route: HashMap<String, Gate> = rl
            .routes
            .iter()
            .map(|r| {
                (
                    r.routing_id.clone(),
                    Gate {
                        semaphore: Arc::new(Semaphore::new(r.max_concurrent)),
                        timeout: r.queue_timeout.unwrap_or(rl.queue_timeout),
                    },
                )
            })
            .collect();

        let per_family: HashMap<String, Gate> = rl
            .families
            .iter()
            .map(|f| {
                (
                    f.family.as_str().to_string(),
                    Gate {
                        semaphore: Arc::new(Semaphore::new(f.max_concurrent)),
                        timeout: f.queue_timeout.unwrap_or(rl.queue_timeout),
                    },
                )
            })
            .collect();

        Some(Self {
            per_route,
            per_family,
            global,
        })
    }

    /// Acquires a concurrency permit for the given route, blocking up to the
    /// configured timeout. Returns `Ok(None)` when the route has no limit.
    pub async fn acquire(&self, route: &ModelRoute) -> Result<Option<Permit>, Denied> {
        let gate = self
            .per_route
            .get(&route.routing_id)
            .or_else(|| self.per_family.get(route.family.as_str()))
            .or(self.global.as_ref());

        let Some(gate) = gate else {
            return Ok(None);
        };

        match tokio::time::timeout(
            gate.timeout,
            Arc::clone(&gate.semaphore).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(Some(Permit(permit))),
            // Semaphore closed — should not happen, but treat as unlimited.
            Ok(Err(_)) => Ok(None),
            Err(_) => Err(Denied::TimedOut),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RateLimitConfig, RateLimitFamilyRule, RateLimitRouteRule};

    fn route(routing_id: &str, family: ModelFamily) -> ModelRoute {
        ModelRoute {
            routing_id: routing_id.to_string(),
            upstream_model: "m".to_string(),
            display_name: "M".to_string(),
            family,
            ..ModelRoute::default()
        }
    }

    fn limiter(rl: RateLimitConfig) -> RateLimiter {
        let config = Config {
            rate_limit: Some(rl),
            ..Config::default()
        };
        RateLimiter::from_config(&config).unwrap()
    }

    #[tokio::test]
    async fn no_config_means_no_limiter() {
        assert!(RateLimiter::from_config(&Config::default()).is_none());
    }

    #[tokio::test]
    async fn global_limit_applies_to_any_route() {
        let lim = limiter(RateLimitConfig {
            max_concurrent: Some(1),
            queue_timeout: Duration::from_millis(1),
            ..RateLimitConfig::default()
        });
        let r = route("gpt-test", ModelFamily::Gpt);
        let p1 = lim.acquire(&r).await.unwrap();
        assert!(p1.is_some());
        // Second acquire should time out (limit is 1, timeout is tiny).
        let result = lim.acquire(&r).await;
        assert!(matches!(result, Err(Denied::TimedOut)));
    }

    #[tokio::test]
    async fn route_override_takes_precedence() {
        let lim = limiter(RateLimitConfig {
            max_concurrent: Some(10),
            routes: vec![RateLimitRouteRule {
                routing_id: "narrow".to_string(),
                max_concurrent: 1,
                queue_timeout: Some(Duration::from_millis(1)),
            }],
            ..RateLimitConfig::default()
        });
        let narrow = route("narrow", ModelFamily::Gpt);
        let wide = route("wide", ModelFamily::Gpt);
        let _p1 = lim.acquire(&narrow).await.unwrap();
        // narrow is full (1 slot); wide still has room (global 10).
        assert!(matches!(lim.acquire(&narrow).await, Err(Denied::TimedOut)));
        assert!(lim.acquire(&wide).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn family_override_takes_precedence_over_global() {
        let lim = limiter(RateLimitConfig {
            max_concurrent: Some(10),
            families: vec![RateLimitFamilyRule {
                family: ModelFamily::Grok,
                max_concurrent: 1,
                queue_timeout: Some(Duration::from_millis(1)),
            }],
            ..RateLimitConfig::default()
        });
        let grok = route("grok-test", ModelFamily::Grok);
        let gpt = route("gpt-test", ModelFamily::Gpt);
        let _p1 = lim.acquire(&grok).await.unwrap();
        assert!(matches!(lim.acquire(&grok).await, Err(Denied::TimedOut)));
        // GPT still uses the global (10 slots).
        assert!(lim.acquire(&gpt).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn dropping_permit_frees_the_slot() {
        let lim = limiter(RateLimitConfig {
            max_concurrent: Some(1),
            queue_timeout: Duration::from_millis(50),
            ..RateLimitConfig::default()
        });
        let r = route("r", ModelFamily::Gpt);
        {
            let _p = lim.acquire(&r).await.unwrap();
        }
        // Slot freed — should succeed.
        assert!(lim.acquire(&r).await.unwrap().is_some());
    }
}
