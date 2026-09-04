//! Context-window discovery and sub-provider pinning for
//! `[[openai-providers]]` routes.
//!
//! A route that opts into `context-window-scaling` needs its real window, and
//! `doctor` reports every route against it; the host already publishes it.
//! Asking the host beats asking the user: the number is provider-specific,
//! changes when a model is upgraded, and a mistyped one moves the compaction
//! point silently.
//!
//! On `OpenRouter` one model is served by many sub-providers with different
//! windows, and routing ignores prompt size. A model with
//! `min-context-window` gets the sub-providers that serve at least that
//! selected here and pinned per request through the child config
//! ([`crate::supervisor::upstream_config_yaml`]).
//!
//! The service fetches at start and caches the answer in the state directory
//! ([`fetch_context_windows`]); both the service and `doctor` then apply the
//! cache ([`apply_cached_windows`]), so a provider outage degrades to the last
//! known answer and `doctor` sees the same numbers the service runs with.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::client_window::client_context_window;
use crate::config::{Config, OpenAiProvider, ProviderModel, is_openrouter};
use crate::state::Dirs;

const CACHE_FILE: &str = "context-windows.json";

/// What the service needs from the host for a model: its window when none
/// is configured, and a sub-provider selection when one is asked for.
fn needs_lookup(model: &ProviderModel) -> bool {
    model.context_window.is_none() || model.min_context_window.is_some()
}

/// The provider models the host has to be asked about.
fn models_to_look_up(config: &Config) -> Vec<(String, String)> {
    config
        .openai_providers
        .iter()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(|model| needs_lookup(model))
                .map(|model| (provider.name.clone(), model.name.clone()))
        })
        .collect()
}

/// The most a service start waits on discovery altogether. It runs before
/// the listener binds, so every Claude request is held behind it; a slow
/// host costs at most this, and the cache covers what did not arrive.
const DISCOVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// One cached answer for a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cached {
    /// The window the host guarantees: across the pinned sub-providers when
    /// `pin` is set, across all of them otherwise.
    pub(crate) window: u64,
    /// The sub-provider selection the last successful lookup produced for
    /// the model's `min-context-window` at the time — or `None` when that
    /// lookup found no qualifying sub-provider (a tombstone that replaces an
    /// older selection) or the model asked for none.
    #[serde(default)]
    pub(crate) pin: Option<Pin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pin {
    /// The `min-context-window` this selection was computed for. A selection
    /// for a higher bar still satisfies a lower one; never the reverse.
    pub(crate) min: u64,
    /// Sorted provider slugs, the values `OpenRouter`'s `provider.only` takes.
    pub(crate) providers: Vec<String>,
}

/// Asks each host about its models and refreshes the cache in the state
/// directory. Never fails the caller: an unreachable host leaves the cache
/// as it was, and [`apply_cached_windows`] works from that. Service start
/// only — `doctor` reads the cache this leaves behind.
pub async fn fetch_context_windows(config: &Config, dirs: &Dirs) {
    let wanted = models_to_look_up(config);
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
            Ok(Ok(answers)) => {
                for (model, answer) in answers {
                    cache.insert(cache_key(&provider.name, &model), answer);
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

/// Fills in each provider model's discovered window and pinned sub-providers
/// from the cache [`fetch_context_windows`] maintains, then re-prepares the
/// config so the generated routes and usage scales reflect them. A model
/// with nothing cached is left as it was, which `doctor` reports.
///
/// Windows: an explicit `context-window` is never touched. A scaling route
/// only takes a window larger than the client's: scaling cannot help below
/// it, and a discovered number must never fail the config the way a
/// hand-written one does — Claude traffic would stop with it. A route that
/// does not scale takes the host's number as-is; it only informs.
///
/// Pins: a cached selection applies only when the model still asks for one
/// and the selection was computed for at least the current
/// `min-context-window`. A model that asks and gets none is not served
/// ([`ProviderModel::is_served`]). A cached window that rests on a
/// selection which no longer applies is not a window for the route as it is
/// now, so it is left unknown too.
///
/// # Errors
/// Returns the error of re-preparing the config, which the applied values
/// themselves cannot cause (windows are positive, and only scaling routes
/// are validated against the declaration).
pub fn apply_cached_windows(config: &mut Config, dirs: &Dirs) -> anyhow::Result<()> {
    if models_to_look_up(config).is_empty() {
        return Ok(());
    }
    let cache = read_cache(&dirs.state_dir.join(CACHE_FILE));
    let believed = client_context_window(config.declared_context_window);
    for provider in &mut config.openai_providers {
        for model in &mut provider.models {
            if !needs_lookup(model) {
                continue;
            }
            let cached = cache.get(&cache_key(&provider.name, &model.name));
            let pin = match (
                model.min_context_window,
                cached.and_then(|c| c.pin.as_ref()),
            ) {
                (Some(min), Some(pin)) if pin.min >= min => Some(pin.providers.clone()),
                (Some(min), _) => {
                    tracing::warn!(
                        model = model.name,
                        min,
                        "no sub-provider selection for at least this window from the \
                         service's last lookup; this model is not served"
                    );
                    None
                }
                (None, _) => None,
            };
            model.pinned_providers = pin;

            // A route that is not served has no window worth applying, and a
            // cached window belongs to the selection it was measured with:
            // with the selection gone, so is the number.
            if model.context_window.is_some() || !model.is_served() {
                continue;
            }
            let window = cached
                .filter(|c| c.pin.is_none() || model.pinned_providers.is_some())
                .map(|c| c.window)
                .filter(|window| *window > 0);
            match window {
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

/// Every configured model's answer for one provider, keyed by the host's
/// model ID. A model whose lookup failed is absent, so its cached answer
/// survives.
async fn discover_provider(
    client: &reqwest::Client,
    provider: &OpenAiProvider,
) -> anyhow::Result<BTreeMap<String, Cached>> {
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
            let answer = look_up(client, base, api_key, model, aggregate).await;
            (model.name.clone(), answer)
        })
    });
    Ok(futures_util::future::join_all(lookups)
        .await
        .into_iter()
        .filter_map(|(model, answer)| Some((model, answer?)))
        .collect())
}

/// The host's answer for one model — its guaranteed window and, when the
/// model asks for one, the sub-provider selection — or `None` when that
/// cannot be established.
///
/// On `OpenRouter` the catalog's number is the *largest* sub-provider window,
/// so when the endpoint lookup fails it is not a fallback: nothing is
/// guaranteed, and a caller that stored it would vouch for a window some
/// sub-provider does not have. Any other host serves one window: the
/// catalog's.
async fn look_up(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &ProviderModel,
    aggregate: u64,
) -> Option<Cached> {
    if !is_openrouter(base) {
        return Some(Cached {
            window: aggregate,
            pin: None,
        });
    }
    let endpoints = openrouter_endpoints(client, base, api_key, &model.name).await?;
    Some(match model.min_context_window {
        Some(min) => match select_providers(&endpoints, min) {
            Some((providers, window)) => Cached {
                window: window.min(aggregate),
                pin: Some(Pin { min, providers }),
            },
            // Looked, found nothing: the tombstone that retires an older
            // selection. The window is what an unpinned route would get.
            None => Cached {
                window: narrowest(&endpoints)?.min(aggregate),
                pin: None,
            },
        },
        None => Cached {
            window: narrowest(&endpoints)?.min(aggregate),
            pin: None,
        },
    })
}

/// The window a host guarantees for `model_id`, given the aggregate its
/// catalog advertises — or `None` when that cannot be established. See
/// [`look_up`]; this is the pin-free view `verify-providers` reports.
pub(crate) async fn host_window(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model_id: &str,
    aggregate: u64,
) -> Option<u64> {
    if !is_openrouter(base) {
        return Some(aggregate);
    }
    let endpoints = openrouter_endpoints(client, base, api_key, model_id).await?;
    narrowest(&endpoints).map(|narrowest| narrowest.min(aggregate))
}

/// One `OpenRouter` endpoint: the sub-provider slug and the window it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The provider slug — the part of the endpoint's `tag` before any `/`
    /// (`atlas-cloud/fp8` → `atlas-cloud`), which is what `provider.only`
    /// and `OpenRouter`'s own error messages name.
    pub(crate) provider: String,
    pub(crate) context_length: u64,
}

/// The sub-providers `OpenRouter` may route `model` to, with their windows.
/// `None` when the endpoints call fails or reports nothing — the caller then
/// leaves everything unknown.
pub(crate) async fn openrouter_endpoints(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &str,
) -> Option<Vec<Endpoint>> {
    let body = client
        .get(format!("{base}/models/{model}/endpoints"))
        .bearer_auth(api_key)
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    let endpoints = parse_endpoints(&body)?;
    (!endpoints.is_empty()).then_some(endpoints)
}

pub(crate) fn parse_endpoints(body: &[u8]) -> Option<Vec<Endpoint>> {
    let document: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(
        document
            .get("data")?
            .get("endpoints")?
            .as_array()?
            .iter()
            .filter_map(|endpoint| {
                let tag = endpoint.get("tag")?.as_str()?;
                let provider = tag.split('/').next()?.trim();
                let context_length = endpoint.get("context_length")?.as_u64()?;
                (!provider.is_empty()).then(|| Endpoint {
                    provider: provider.to_string(),
                    context_length,
                })
            })
            .collect(),
    )
}

/// The smallest window across every endpoint — the only one an unpinned
/// request is sure to get.
pub(crate) fn narrowest(endpoints: &[Endpoint]) -> Option<u64> {
    endpoints.iter().map(|e| e.context_length).min()
}

/// Each sub-provider's narrowest endpoint window — the window a request
/// routed to that provider is sure to get.
pub(crate) fn narrowest_by_provider(endpoints: &[Endpoint]) -> BTreeMap<String, u64> {
    let mut windows: BTreeMap<String, u64> = BTreeMap::new();
    for endpoint in endpoints {
        windows
            .entry(endpoint.provider.clone())
            .and_modify(|window| *window = (*window).min(endpoint.context_length))
            .or_insert(endpoint.context_length);
    }
    windows
}

/// The sub-providers whose *every* endpoint serves at least `min`, sorted,
/// with the smallest window among them; `None` when no provider qualifies.
/// `provider.only` selects providers, not endpoints, so a provider with one
/// narrow endpoint could still route narrow and is left out whole.
pub(crate) fn select_providers(endpoints: &[Endpoint], min: u64) -> Option<(Vec<String>, u64)> {
    let qualifying: Vec<(String, u64)> = narrowest_by_provider(endpoints)
        .into_iter()
        .filter(|(_, window)| *window >= min)
        .collect();
    let window = qualifying.iter().map(|(_, window)| *window).min()?;
    Some((
        qualifying
            .into_iter()
            .map(|(provider, _)| provider)
            .collect(),
        window,
    ))
}

/// The cache as written by older releases (a bare window) or this one.
#[derive(Deserialize)]
#[serde(untagged)]
enum CacheEntry {
    Bare(u64),
    Full(Cached),
}

fn read_cache(path: &Path) -> BTreeMap<String, Cached> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<BTreeMap<String, CacheEntry>>(&contents).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(key, entry)| {
            let cached = match entry {
                CacheEntry::Bare(window) => Cached { window, pin: None },
                CacheEntry::Full(cached) => cached,
            };
            (key, cached)
        })
        .collect()
}

fn write_cache(path: &Path, cache: &BTreeMap<String, Cached>) {
    if let Ok(contents) = serde_json::to_string_pretty(cache)
        && let Err(error) = std::fs::write(path, contents)
    {
        tracing::warn!(%error, "failed to cache discovered context windows");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(window: u64) -> Cached {
        Cached { window, pin: None }
    }

    #[test]
    fn the_cache_key_cannot_collide_across_providers() {
        assert_ne!(cache_key("a", "b/c"), cache_key("a/b", "c"));
    }

    #[test]
    fn a_missing_or_corrupt_cache_reads_as_empty_and_old_entries_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context-windows.json");
        assert!(read_cache(&path).is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_cache(&path).is_empty());
        // The shape a 0.1.17 service wrote.
        std::fs::write(&path, r#"{"k": 7}"#).unwrap();
        assert_eq!(read_cache(&path).get("k"), Some(&window(7)));
        let pinned = Cached {
            window: 1_048_576,
            pin: Some(Pin {
                min: 1_000_000,
                providers: vec!["decart".to_string(), "fireworks".to_string()],
            }),
        };
        write_cache(&path, &BTreeMap::from([("k".to_string(), pinned.clone())]));
        assert_eq!(read_cache(&path).get("k"), Some(&pinned));
    }

    /// The real GLM-5.2 endpoint list (2026-09-04), reduced to the two
    /// fields the selection reads.
    const GLM_ENDPOINTS: &str = include_str!("fixtures/openrouter-glm-5.2-endpoints.json");

    #[test]
    fn selection_keeps_providers_whose_every_endpoint_meets_the_bar() {
        let endpoints = parse_endpoints(GLM_ENDPOINTS.as_bytes()).unwrap();
        assert_eq!(endpoints.len(), 33);
        assert_eq!(narrowest(&endpoints), Some(202_752));

        let (providers, window) = select_providers(&endpoints, 1_000_000).unwrap();
        assert_eq!(window, 1_000_000, "venice serves exactly 1M");
        for excluded in [
            "ambient",
            "cloudflare",
            "digitalocean",
            "parasail",
            "reka",
            "together",
        ] {
            assert!(!providers.contains(&excluded.to_string()), "{excluded}");
        }
        for included in ["baidu", "decart", "fireworks", "venice", "z-ai"] {
            assert!(providers.contains(&included.to_string()), "{included}");
        }
        assert!(providers.windows(2).all(|pair| pair[0] < pair[1]), "sorted");

        // A bar above every window: nothing.
        assert!(select_providers(&endpoints, 2_000_000).is_none());
        // A low bar keeps everyone and guarantees the narrowest.
        let (all, window) = select_providers(&endpoints, 100_000).unwrap();
        assert_eq!(all.len(), 25);
        assert_eq!(window, 202_752);
    }

    #[test]
    fn a_provider_with_one_narrow_endpoint_is_left_out_whole() {
        let endpoints = vec![
            Endpoint {
                provider: "wide".into(),
                context_length: 1_048_576,
            },
            Endpoint {
                provider: "mixed".into(),
                context_length: 1_048_576,
            },
            Endpoint {
                provider: "mixed".into(),
                context_length: 8_192,
            },
        ];
        let (providers, window) = select_providers(&endpoints, 1_000_000).unwrap();
        assert_eq!(providers, vec!["wide".to_string()]);
        assert_eq!(window, 1_048_576);
    }

    #[test]
    fn endpoint_tags_reduce_to_provider_slugs() {
        let body = br#"{"data":{"endpoints":[
            {"tag":"atlas-cloud/fp8","context_length":1048576},
            {"tag":"cloudflare","context_length":262144},
            {"tag":"","context_length":1},
            {"context_length":5},
            {"tag":"x/y","context_length":"nope"}
        ]}}"#;
        let endpoints = parse_endpoints(body).unwrap();
        assert_eq!(
            endpoints,
            vec![
                Endpoint {
                    provider: "atlas-cloud".into(),
                    context_length: 1_048_576
                },
                Endpoint {
                    provider: "cloudflare".into(),
                    context_length: 262_144
                },
            ]
        );
        assert!(parse_endpoints(b"{}").is_none());
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
                (
                    cache_key("openrouter", "moonshotai/kimi-k3"),
                    window(1_048_576),
                ),
                (cache_key("openrouter", "z-ai/glm-5.2"), window(202_752)),
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
            &BTreeMap::from([(
                cache_key("openrouter", "moonshotai/kimi-k3"),
                window(200_000),
            )]),
        );
        let mut config = two_route_config();
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(route_window(&config, "kimi-k3"), None);
    }

    fn pinned_config(min: Option<u64>, explicit_window: Option<u64>) -> Config {
        let min = min.map_or(String::new(), |min| format!("min-context-window = {min}\n"));
        let window = explicit_window.map_or(String::new(), |window| {
            format!("context-window = {window}\n")
        });
        let mut config: Config = toml::from_str(&format!(
            r#"
declared-context-window = 250000
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "z-ai/glm-5.2"
routing-id = "glm-5.2"
display-name = "GLM-5.2"
{min}{window}"#
        ))
        .unwrap();
        config.prepare().unwrap();
        config
    }

    fn glm(config: &Config) -> &ProviderModel {
        &config.openai_providers[0].models[0]
    }

    #[test]
    fn a_cached_pin_applies_only_to_the_bar_it_was_computed_for_or_a_lower_one() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = dirs_in(dir.path());
        let selection = Cached {
            window: 1_048_576,
            pin: Some(Pin {
                min: 1_000_000,
                providers: vec!["decart".into(), "fireworks".into()],
            }),
        };
        write_cache(
            &dir.path().join(CACHE_FILE),
            &BTreeMap::from([(cache_key("openrouter", "z-ai/glm-5.2"), selection)]),
        );

        // Same bar: pinned, window from the selection.
        let mut config = pinned_config(Some(1_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(
            glm(&config).pinned_providers,
            Some(vec!["decart".to_string(), "fireworks".to_string()])
        );
        assert!(glm(&config).is_served());
        assert_eq!(route_window(&config, "glm-5.2"), Some(1_048_576));
        let route = config
            .effective_models()
            .find(|route| route.routing_id == "glm-5.2")
            .unwrap();
        assert_eq!(route.min_context_window, Some(1_000_000));
        assert_eq!(route.pinned_providers.as_ref().map(Vec::len), Some(2));

        // Lower bar: the old selection still satisfies it.
        let mut config = pinned_config(Some(500_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(glm(&config).is_served());

        // Raised bar: fail closed — no pin, no window, not served.
        let mut config = pinned_config(Some(2_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(glm(&config).pinned_providers, None);
        assert!(!glm(&config).is_served());
        assert_eq!(route_window(&config, "glm-5.2"), None);

        // Field removed: served unpinned, but the pinned window is not this
        // route's window any more.
        let mut config = pinned_config(None, None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(glm(&config).is_served());
        assert_eq!(glm(&config).pinned_providers, None);
        assert_eq!(route_window(&config, "glm-5.2"), None);

        // Explicit window plus a bar: the window stays explicit, the pin
        // still applies.
        let mut config = pinned_config(Some(1_000_000), Some(1_048_576));
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(glm(&config).is_served());
        assert_eq!(route_window(&config, "glm-5.2"), Some(1_048_576));

        // Tombstone (a lookup that found nothing): not served.
        write_cache(
            &dir.path().join(CACHE_FILE),
            &BTreeMap::from([(cache_key("openrouter", "z-ai/glm-5.2"), window(202_752))]),
        );
        let mut config = pinned_config(Some(1_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(!glm(&config).is_served());
        assert_eq!(route_window(&config, "glm-5.2"), None);

        // Nothing cached at all: not served either.
        std::fs::remove_file(dir.path().join(CACHE_FILE)).unwrap();
        let mut config = pinned_config(Some(1_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(!glm(&config).is_served());
    }
}
