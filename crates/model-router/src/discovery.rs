//! Context-window discovery for `[[openai-providers]]` routes.
//!
//! A route that opts into `context-window-scaling` needs its real window, and
//! `doctor` reports every route against it; the host already publishes it.
//! Asking the host beats asking the user: the number is provider-specific,
//! changes when a model is upgraded, and a mistyped one moves the compaction
//! point silently.
//!
//! The service fetches at start and caches the answer in the state directory
//! ([`fetch_context_windows`]); both the service and `doctor` then apply the
//! cache ([`apply_cached_windows`]), so a provider outage degrades to the last
//! known window and `doctor` sees the same numbers the service runs with.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client_window::client_context_window;
use crate::config::{Config, OpenAiProvider};
use crate::state::Dirs;

const CACHE_FILE: &str = "context-windows.json";

/// `OpenRouter` fans one model slug out across sub-providers whose windows can
/// differ, and the aggregate `/models` entry reports the largest. Since a
/// request may land on any of them, only the smallest is safe to scale
/// against.
const OPENROUTER_HOST: &str = "openrouter.ai";

/// The provider models whose window the host has to supply: every
/// `[[openai-providers.models]]` entry without an explicit `context-window`.
/// Not only scaling routes — `doctor` reports every route against the host's
/// number, and a route sized by a `behavesAs` picker row is checked against
/// it.
fn undeclared_models(config: &Config) -> Vec<(String, String)> {
    config
        .openai_providers
        .iter()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(|model| model.context_window.is_none())
                .map(|model| (provider.name.clone(), model.name.clone()))
        })
        .collect()
}

/// The most a service start waits on discovery altogether. It runs before
/// the listener binds, so every Claude request is held behind it; a slow
/// host costs at most this, and the cache covers what did not arrive.
const DISCOVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Asks each host for the windows of its undeclared models and refreshes the
/// cache in the state directory. Never fails the caller: an unreachable host
/// leaves the cache as it was, and [`apply_cached_windows`] works from that.
/// Service start only — `doctor` reads the cache this leaves behind.
pub async fn fetch_context_windows(config: &Config, dirs: &Dirs) {
    let wanted = undeclared_models(config);
    if wanted.is_empty() {
        return;
    }

    let cache_path = dirs.state_dir.join(CACHE_FILE);
    let mut cache = read_cache(&cache_path);
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "context-window discovery client unavailable");
            return;
        }
    };

    let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    for provider in &config.openai_providers {
        if !wanted.iter().any(|(name, _)| name == &provider.name) {
            continue;
        }
        match tokio::time::timeout_at(deadline, discover_provider(&client, provider)).await {
            Ok(Ok(windows)) => {
                for (model, window) in windows {
                    cache.insert(cache_key(&provider.name, &model), window);
                }
            }
            Ok(Err(error)) => tracing::warn!(
                provider = provider.name,
                %error,
                "context-window discovery failed; falling back to the cached windows"
            ),
            Err(_) => {
                tracing::warn!(
                    provider = provider.name,
                    "context-window discovery ran past its {}s deadline; falling back to the \
                     cached windows",
                    DISCOVERY_DEADLINE.as_secs()
                );
                break;
            }
        }
    }
    write_cache(&cache_path, &cache);
}

/// Fills in `context-window` for every undeclared provider model from the
/// cache [`fetch_context_windows`] maintains, then re-prepares the config so
/// the usage scales reflect the windows. A route with no cached window is
/// left as it was, which `doctor` reports.
///
/// A scaling route only takes a window larger than the client's: scaling
/// cannot help below it, and a discovered number must never fail the config
/// the way a hand-written one does — Claude traffic would stop with it. A
/// route that does not scale takes the host's number as-is; it only informs.
///
/// # Errors
/// Returns the error of re-preparing the config, which the applied windows
/// themselves cannot cause (they are positive, and only scaling routes are
/// validated against the declaration).
pub fn apply_cached_windows(config: &mut Config, dirs: &Dirs) -> anyhow::Result<()> {
    if undeclared_models(config).is_empty() {
        return Ok(());
    }
    let cache = read_cache(&dirs.state_dir.join(CACHE_FILE));
    let believed = client_context_window(config.declared_context_window);
    for provider in &mut config.openai_providers {
        for model in &mut provider.models {
            if model.context_window.is_some() {
                continue;
            }
            let cached = cache
                .get(&cache_key(&provider.name, &model.name))
                .copied()
                .filter(|window| *window > 0);
            match cached {
                Some(window) if !model.context_window_scaling || window > believed => {
                    tracing::info!(model = model.name, window, "discovered context window");
                    model.context_window = Some(window);
                }
                Some(window) => tracing::warn!(
                    model = model.name,
                    window,
                    believed,
                    "discovered context window is not larger than the one Claude Code already \
                     believes; leaving this route unscaled"
                ),
                None if model.context_window_scaling => tracing::warn!(
                    model = model.name,
                    "no context window discovered; this route will not be scaled — set \
                     `context-window` explicitly to scale it anyway"
                ),
                None => {}
            }
        }
    }
    config.prepare()
}

fn cache_key(provider: &str, model: &str) -> String {
    format!("{provider}\u{1f}{model}")
}

/// Every configured model's window for one provider, keyed by the host's
/// model ID.
async fn discover_provider(
    client: &reqwest::Client,
    provider: &OpenAiProvider,
) -> anyhow::Result<BTreeMap<String, u64>> {
    let api_key = provider
        .api_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no api-key configured"))?;
    let base = provider.base_url.trim_end_matches('/');
    let body = client
        .get(format!("{base}/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("{}", crate::verify::scrub_key(&error, api_key)))?
        .bytes()
        .await?;
    let catalog = crate::verify::parse_catalog(&body)
        .ok_or_else(|| anyhow::anyhow!("provider /models response has no data[].id list"))?;

    // One lookup per model, side by side: on OpenRouter each is its own
    // request, and a stalled one must not serialise behind the others.
    let lookups = provider.models.iter().filter_map(|model| {
        let aggregate = catalog
            .iter()
            .find(|entry| entry.id == model.name)
            .and_then(|entry| entry.context_length)?;
        Some(async move {
            let window = host_window(client, base, api_key, &model.name, aggregate).await;
            (model.name.clone(), window)
        })
    });
    Ok(futures_util::future::join_all(lookups)
        .await
        .into_iter()
        .filter_map(|(model, window)| Some((model, window?)))
        .collect())
}

/// The window a host actually guarantees for `model_id`, given the aggregate
/// its catalog advertises — or `None` when that cannot be established.
///
/// On `OpenRouter` those differ: provider routing is not documented to
/// consider prompt size, so a request can land on any sub-provider and only
/// the narrowest is safe. The aggregate is the *largest* of them, so when the
/// per-model lookup fails it is not a fallback: nothing is guaranteed, and a
/// caller that stored the aggregate would vouch for a window some
/// sub-provider does not have. Restricting providers account-side is what
/// unlocks the advertised number, and `context-window` then overrides this.
pub(crate) async fn host_window(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model_id: &str,
    aggregate: u64,
) -> Option<u64> {
    if !base.contains(OPENROUTER_HOST) {
        return Some(aggregate);
    }
    openrouter_min_window(client, base, api_key, model_id)
        .await
        .map(|narrowest| narrowest.min(aggregate))
}

/// The smallest `context_length` among the sub-providers `OpenRouter` may route
/// this model to. `None` when the endpoints call fails or reports nothing —
/// the caller then leaves the window unknown.
async fn openrouter_min_window(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &str,
) -> Option<u64> {
    let body = client
        .get(format!("{base}/models/{model}/endpoints"))
        .bearer_auth(api_key)
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    let document: serde_json::Value = serde_json::from_slice(&body).ok()?;
    document
        .get("data")?
        .get("endpoints")?
        .as_array()?
        .iter()
        .filter_map(|endpoint| {
            endpoint
                .get("context_length")
                .and_then(serde_json::Value::as_u64)
        })
        .min()
}

fn read_cache(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Path, cache: &BTreeMap<String, u64>) {
    if let Ok(contents) = serde_json::to_string_pretty(cache)
        && let Err(error) = std::fs::write(path, contents)
    {
        tracing::warn!(%error, "failed to cache discovered context windows");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_cannot_collide_across_providers() {
        assert_ne!(cache_key("a", "b/c"), cache_key("a/b", "c"));
    }

    #[test]
    fn a_missing_or_corrupt_cache_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context-windows.json");
        assert!(read_cache(&path).is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_cache(&path).is_empty());
        write_cache(&path, &BTreeMap::from([("k".to_string(), 7)]));
        assert_eq!(read_cache(&path).get("k"), Some(&7));
    }

    fn dirs_in(dir: &Path) -> Dirs {
        Dirs {
            config_dir: dir.join("config"),
            state_dir: dir.to_path_buf(),
            cache_dir: dir.join("cache"),
        }
    }

    fn two_route_config() -> Config {
        let mut config: Config = toml::from_str(
            r#"
declared-context-window = 250000
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
context-window-scaling = true
[[openai-providers.models]]
name = "z-ai/glm-5.2"
routing-id = "glm-5.2"
display-name = "GLM-5.2"
"#,
        )
        .unwrap();
        config.prepare().unwrap();
        config
    }

    fn route_window(config: &Config, routing_id: &str) -> Option<u64> {
        config
            .effective_models()
            .find(|route| route.routing_id == routing_id)
            .unwrap()
            .context_window
    }

    #[test]
    fn cached_windows_reach_every_undeclared_route_and_scale_only_above_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = dirs_in(dir.path());
        let mut config = two_route_config();
        // No cache yet: nothing changes and nothing fails.
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(route_window(&config, "kimi-k3"), None);
        assert_eq!(route_window(&config, "glm-5.2"), None);

        write_cache(
            &dir.path().join(CACHE_FILE),
            &BTreeMap::from([
                (cache_key("openrouter", "moonshotai/kimi-k3"), 1_048_576),
                (cache_key("openrouter", "z-ai/glm-5.2"), 202_752),
            ]),
        );
        let mut config = two_route_config();
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(route_window(&config, "kimi-k3"), Some(1_048_576));
        // The plain route takes the host's number even below the client's
        // window: it is what doctor has to warn about.
        assert_eq!(route_window(&config, "glm-5.2"), Some(202_752));
        let kimi = config
            .effective_models()
            .find(|route| route.routing_id == "kimi-k3")
            .unwrap();
        assert!(kimi.usage_scale.is_some(), "re-prepare computes the scale");

        // A scaling route never takes a window at or below the client's: the
        // config would have refused it hand-written.
        write_cache(
            &dir.path().join(CACHE_FILE),
            &BTreeMap::from([(cache_key("openrouter", "moonshotai/kimi-k3"), 200_000)]),
        );
        let mut config = two_route_config();
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(route_window(&config, "kimi-k3"), None);
    }
}
