//! Context-window discovery for `[[openai-providers]]` routes.
//!
//! A route that opts into `context-window-scaling` needs its real window, and
//! the host already publishes it. Asking the host beats asking the user: the
//! number is provider-specific, changes when a model is upgraded, and a
//! mistyped one moves the compaction point silently.
//!
//! The answer is cached in the state directory so a provider outage degrades
//! to the last known window rather than to no scaling at all.

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

/// Fills in `context-window` for every scaling route that did not configure
/// one explicitly. Never fails the caller: a route left without a window is
/// simply not scaled (its model gets the window Claude Code already believes
/// it has), which `doctor` reports.
pub async fn fill_context_windows(config: &mut Config, dirs: &Dirs) {
    let wanted: Vec<(String, String)> = config
        .openai_providers
        .iter()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(|model| model.context_window_scaling && model.context_window.is_none())
                .map(|model| (provider.name.clone(), model.name.clone()))
        })
        .collect();
    if wanted.is_empty() {
        return;
    }

    let cache_path = dirs.state_dir.join(CACHE_FILE);
    let mut cache = read_cache(&cache_path);
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "context-window discovery client unavailable");
            return;
        }
    };

    for provider in &config.openai_providers {
        if !wanted.iter().any(|(name, _)| name == &provider.name) {
            continue;
        }
        match discover_provider(&client, provider).await {
            Ok(windows) => {
                for (model, window) in windows {
                    cache.insert(cache_key(&provider.name, &model), window);
                }
            }
            Err(error) => tracing::warn!(
                provider = provider.name,
                %error,
                "context-window discovery failed; falling back to the cached windows"
            ),
        }
    }
    write_cache(&cache_path, &cache);

    let declared = config.declared_context_window;
    for provider in &mut config.openai_providers {
        for model in &mut provider.models {
            if !model.context_window_scaling || model.context_window.is_some() {
                continue;
            }
            let believed = client_context_window(&model.routing_id, declared);
            match cache.get(&cache_key(&provider.name, &model.name)).copied() {
                // Only a window larger than the client's is scalable, and a
                // discovered number must never fail the config the way a
                // hand-written one does: Claude traffic would stop with it.
                Some(window) if window > believed => {
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
                None => tracing::warn!(
                    model = model.name,
                    "no context window discovered; this route will not be scaled — set \
                     `context-window` explicitly to scale it anyway"
                ),
            }
        }
    }
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

    let mut windows = BTreeMap::new();
    for model in &provider.models {
        let Some(aggregate) = catalog
            .iter()
            .find(|entry| entry.id == model.name)
            .and_then(|entry| entry.context_length)
        else {
            continue;
        };
        windows.insert(
            model.name.clone(),
            host_window(client, base, api_key, &model.name, aggregate).await,
        );
    }
    Ok(windows)
}

/// The window a host actually guarantees for `model_id`, given the aggregate
/// its catalog advertises.
///
/// On `OpenRouter` those differ: provider routing is not documented to
/// consider prompt size, so a request can land on any sub-provider and only
/// the narrowest is safe. Restricting providers account-side is what unlocks
/// the advertised number, and `context-window` then overrides this.
pub(crate) async fn host_window(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model_id: &str,
    aggregate: u64,
) -> u64 {
    if !base.contains(OPENROUTER_HOST) {
        return aggregate;
    }
    openrouter_min_window(client, base, api_key, model_id)
        .await
        .map_or(aggregate, |narrowest| narrowest.min(aggregate))
}

/// The smallest `context_length` among the sub-providers `OpenRouter` may route
/// this model to. `None` when the endpoints call fails or reports nothing —
/// the caller then keeps the aggregate.
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
}
