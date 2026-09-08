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

/// Whether any provider model needs the host asked about it.
fn needs_discovery(config: &Config) -> bool {
    config
        .openai_providers
        .iter()
        .flat_map(|provider| &provider.models)
        .any(needs_lookup)
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

/// The window a host guarantees for a model, from its `OpenRouter` endpoint
/// list and the catalog aggregate (never more than that), with the
/// sub-provider selection when `min` asks for one and some provider
/// qualifies. Without a qualifying selection the window is the unpinned
/// narrowest; `None` when that cannot be established. The one rule the
/// service pins by and `verify-providers` reports.
pub(crate) fn guaranteed_window(
    endpoints: &[Endpoint],
    min: Option<u64>,
    aggregate: u64,
) -> (Option<u64>, Option<Pin>) {
    if let Some(min) = min
        && let Some((providers, window)) = select_providers(endpoints, min)
    {
        return (Some(window.min(aggregate)), Some(Pin { min, providers }));
    }
    (narrowest(endpoints).map(|n| n.min(aggregate)), None)
}

/// Asks each host about its models and refreshes the cache in the state
/// directory. Never fails the caller: an unreachable host leaves the cache
/// as it was, and [`apply_cached_windows`] works from that. Service start
/// only — `doctor` reads the cache this leaves behind.
pub async fn fetch_context_windows(config: &Config, dirs: &Dirs) {
    if !needs_discovery(config) {
        return;
    }

    let cache_path = dirs.state_dir.join(CACHE_FILE);
    let mut cache = read_cache(&cache_path);
    let client = match provider_client(std::time::Duration::from_secs(15)) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "context-window discovery client unavailable");
            return;
        }
    };

    let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    for provider in &config.openai_providers {
        if !provider.models.iter().any(needs_lookup) {
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
/// with nothing cached is left as it was, which `doctor` reports. An
/// explicit `context-window` is never touched.
///
/// # Errors
/// Returns the error of re-preparing the config, which the applied values
/// themselves cannot cause (windows are positive, and only scaling routes
/// are validated against the declaration).
pub fn apply_cached_windows(config: &mut Config, dirs: &Dirs) -> anyhow::Result<()> {
    if !needs_discovery(config) {
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
            // A scaling route never takes a window at or below the client's:
            // scaling cannot help there, and a discovered number must never
            // fail the config the way a hand-written one does.
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

pub(crate) fn cache_key(provider: &str, model: &str) -> String {
    format!("{provider}\u{1f}{model}")
}

/// The HTTP client the service and `verify-providers` talk to provider hosts
/// with. No redirects: reqwest strips Authorization on cross-host hops
/// anyway, which would surface as a baffling 401 — fail loudly instead.
///
/// # Errors
/// Returns reqwest's error when the client cannot be built.
pub(crate) fn provider_client(timeout: std::time::Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(timeout)
        .build()
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
    let catalog = crate::verify::fetch_catalog(client, provider, api_key).await?;

    // One lookup per model, side by side: on OpenRouter each is its own
    // request, and a stalled one must not serialise behind the others.
    let lookups = provider
        .models
        .iter()
        .filter(|model| needs_lookup(model))
        .filter_map(|model| {
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
    let endpoints = match openrouter_endpoints(client, base, api_key, &model.name).await {
        Ok(endpoints) => endpoints,
        Err(error) => {
            tracing::warn!(
                model = model.name,
                %error,
                "sub-provider lookup failed; this model keeps its cached answer, if any"
            );
            return None;
        }
    };
    // Without a qualifying selection the answer is the tombstone that
    // retires an older one: the window an unpinned route would get.
    let (window, pin) = guaranteed_window(&endpoints, model.min_context_window, aggregate);
    Some(Cached {
        window: window?,
        pin,
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
    let endpoints = openrouter_endpoints(client, base, api_key, model_id)
        .await
        .ok()?;
    guaranteed_window(&endpoints, None, aggregate).0
}

/// One `OpenRouter` endpoint: the sub-provider slug and the window it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The provider slug — the part of the endpoint's `tag` before any `/`
    /// (`atlas-cloud/fp8` → `atlas-cloud`), which is what `provider.only`
    /// and `OpenRouter`'s own error messages name.
    pub(crate) provider: String,
    /// `None` when the host did not report a usable number for this
    /// endpoint. Unknown is treated as unsafe everywhere: it disqualifies
    /// its provider from a pin and leaves the unpinned window unknown.
    pub(crate) context_length: Option<u64>,
}

/// The sub-providers `OpenRouter` may route `model` to, with their windows.
///
/// # Errors
/// Returns an error, scrubbed of the key, when the endpoints call fails or
/// reports nothing usable; the caller then leaves everything unknown.
pub(crate) async fn openrouter_endpoints(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &str,
) -> anyhow::Result<Vec<Endpoint>> {
    let response = client
        .get(format!("{base}/models/{model}/endpoints"))
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| {
            anyhow::anyhow!("{}", crate::verify::scrub(&error.to_string(), api_key))
        })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|error| {
        anyhow::anyhow!("{}", crate::verify::scrub(&error.to_string(), api_key))
    })?;
    anyhow::ensure!(status.is_success(), "endpoints call returned HTTP {status}");
    let endpoints = parse_endpoints(&body).ok_or_else(|| {
        anyhow::anyhow!(
            "endpoints response has no data.endpoints list, or an endpoint without a tag"
        )
    })?;
    anyhow::ensure!(
        !endpoints.is_empty(),
        "endpoints response lists no endpoints"
    );
    Ok(endpoints)
}

/// Every endpoint in the document, or `None` when one of them cannot be
/// attributed to a provider: a request could land on it, so a list that
/// leaves it out would vouch for routing it does not describe.
pub(crate) fn parse_endpoints(body: &[u8]) -> Option<Vec<Endpoint>> {
    let document: serde_json::Value = serde_json::from_slice(body).ok()?;
    document
        .get("data")?
        .get("endpoints")?
        .as_array()?
        .iter()
        .map(|endpoint| {
            let tag = endpoint.get("tag")?.as_str()?;
            let provider = tag.split('/').next()?.trim();
            (!provider.is_empty()).then(|| Endpoint {
                provider: provider.to_string(),
                context_length: endpoint
                    .get("context_length")
                    .and_then(serde_json::Value::as_u64),
            })
        })
        .collect()
}

/// The smallest window across every endpoint — the only one an unpinned
/// request is sure to get; `None` when any endpoint's window is unknown.
pub(crate) fn narrowest(endpoints: &[Endpoint]) -> Option<u64> {
    // `None` orders below every `Some`, so one unknown window wins the min.
    endpoints.iter().map(|e| e.context_length).min().flatten()
}

/// Each sub-provider's narrowest endpoint window — the window a request
/// routed to that provider is sure to get — or `None` when one of its
/// endpoints has no known window.
pub(crate) fn narrowest_by_provider(endpoints: &[Endpoint]) -> BTreeMap<String, Option<u64>> {
    let mut windows: BTreeMap<String, Option<u64>> = BTreeMap::new();
    for endpoint in endpoints {
        let window = windows
            .entry(endpoint.provider.clone())
            .or_insert(Some(u64::MAX));
        *window = (*window).min(endpoint.context_length);
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
        .filter_map(|(provider, window)| Some((provider, window?)))
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

/// The cache, or empty when there is none yet. A cache that cannot be read
/// or parsed also reads as empty, with a warning: the service refetches, and
/// until that succeeds every pinned model is unserved.
fn read_cache(path: &Path) -> BTreeMap<String, Cached> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read the context-window cache");
            return BTreeMap::new();
        }
    };
    serde_json::from_str(&contents).unwrap_or_else(|error| {
        tracing::warn!(path = %path.display(), %error, "ignoring an unparseable context-window cache");
        BTreeMap::new()
    })
}

pub(crate) fn write_cache(path: &Path, cache: &BTreeMap<String, Cached>) {
    let contents = serde_json::to_string_pretty(cache).expect("the cache is plain data");
    if let Err(error) = crate::state::write_private_atomic(path, contents.as_bytes()) {
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
    fn a_missing_or_corrupt_cache_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context-windows.json");
        assert!(read_cache(&path).is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_cache(&path).is_empty());
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
                context_length: Some(1_048_576),
            },
            Endpoint {
                provider: "mixed".into(),
                context_length: Some(1_048_576),
            },
            Endpoint {
                provider: "mixed".into(),
                context_length: Some(8_192),
            },
        ];
        let (providers, window) = select_providers(&endpoints, 1_000_000).unwrap();
        assert_eq!(providers, vec!["wide".to_string()]);
        assert_eq!(window, 1_048_576);
    }

    #[test]
    fn an_endpoint_with_no_known_window_disqualifies_its_provider_and_the_unpinned_window() {
        let endpoints = vec![
            Endpoint {
                provider: "wide".into(),
                context_length: Some(1_048_576),
            },
            Endpoint {
                provider: "shady".into(),
                context_length: Some(1_048_576),
            },
            Endpoint {
                provider: "shady".into(),
                context_length: None,
            },
        ];
        let (providers, window) = select_providers(&endpoints, 1_000_000).unwrap();
        assert_eq!(providers, vec!["wide".to_string()]);
        assert_eq!(window, 1_048_576);
        assert_eq!(narrowest(&endpoints), None);
        assert_eq!(narrowest_by_provider(&endpoints)["shady"], None);
    }

    #[test]
    fn endpoint_tags_reduce_to_provider_slugs() {
        let body = br#"{"data":{"endpoints":[
            {"tag":"atlas-cloud/fp8","context_length":1048576},
            {"tag":"cloudflare","context_length":262144},
            {"tag":"x/y","context_length":"nope"}
        ]}}"#;
        let endpoints = parse_endpoints(body).unwrap();
        assert_eq!(
            endpoints,
            vec![
                Endpoint {
                    provider: "atlas-cloud".into(),
                    context_length: Some(1_048_576)
                },
                Endpoint {
                    provider: "cloudflare".into(),
                    context_length: Some(262_144)
                },
                Endpoint {
                    provider: "x".into(),
                    context_length: None
                },
            ]
        );
        // An endpoint that cannot be attributed to a provider fails the
        // whole list: a request could still land on it.
        for unattributable in [
            br#"{"data":{"endpoints":[{"tag":"","context_length":1}]}}"#.as_slice(),
            br#"{"data":{"endpoints":[{"context_length":5}]}}"#.as_slice(),
        ] {
            assert!(parse_endpoints(unattributable).is_none());
        }
        assert!(parse_endpoints(b"{}").is_none());
    }

    fn two_route_config() -> Config {
        crate::config::parse_and_prepare(
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
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(&dirs.state_dir).unwrap();
        let mut config = two_route_config();
        // No cache yet: nothing changes and nothing fails.
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert_eq!(route_window(&config, "kimi-k3"), None);
        assert_eq!(route_window(&config, "glm-5.2"), None);

        write_cache(
            &dirs.state_dir.join(CACHE_FILE),
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
            &dirs.state_dir.join(CACHE_FILE),
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
        crate::config::parse_and_prepare(&format!(
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
    }

    fn glm(config: &Config) -> &ProviderModel {
        &config.openai_providers[0].models[0]
    }

    #[test]
    fn a_cached_pin_applies_only_to_the_bar_it_was_computed_for_or_a_lower_one() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(&dirs.state_dir).unwrap();
        let selection = Cached {
            window: 1_048_576,
            pin: Some(Pin {
                min: 1_000_000,
                providers: vec!["decart".into(), "fireworks".into()],
            }),
        };
        write_cache(
            &dirs.state_dir.join(CACHE_FILE),
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
            &dirs.state_dir.join(CACHE_FILE),
            &BTreeMap::from([(cache_key("openrouter", "z-ai/glm-5.2"), window(202_752))]),
        );
        let mut config = pinned_config(Some(1_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(!glm(&config).is_served());
        assert_eq!(route_window(&config, "glm-5.2"), None);

        // Nothing cached at all: not served either.
        std::fs::remove_file(dirs.state_dir.join(CACHE_FILE)).unwrap();
        let mut config = pinned_config(Some(1_000_000), None);
        apply_cached_windows(&mut config, &dirs).unwrap();
        assert!(!glm(&config).is_served());
    }

    /// A fake OpenAI-compatible host: `/models` answers from `catalog` and
    /// rejects any other key with 401.
    async fn fake_host(catalog: &'static str) -> String {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        async fn models(
            State(catalog): State<&'static str>,
            headers: HeaderMap,
        ) -> (StatusCode, &'static str) {
            let bearer = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok());
            if bearer == Some("Bearer good-key") {
                (StatusCode::OK, catalog)
            } else {
                (StatusCode::UNAUTHORIZED, r#"{"error":"bad key"}"#)
            }
        }
        let app = axum::Router::new()
            .route("/v1/models", axum::routing::get(models))
            .with_state(catalog);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/v1")
    }

    #[tokio::test]
    async fn discovery_writes_what_apply_reads_and_a_rejected_key_keeps_the_old_answer() {
        let base_url =
            fake_host(r#"{"data":[{"id":"vendor/model","context_length":131072}]}"#).await;
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(&dirs.state_dir).unwrap();
        let provider = |api_key: &str| Config {
            openai_providers: vec![OpenAiProvider {
                name: "host".to_string(),
                base_url: base_url.clone(),
                models: vec![ProviderModel {
                    name: "vendor/model".to_string(),
                    routing_id: "model".to_string(),
                    display_name: "Model".to_string(),
                    ..ProviderModel::default()
                }],
                api_key: Some(api_key.to_string()),
            }],
            ..Config::default()
        };
        let cached = || read_cache(&dirs.state_dir.join(CACHE_FILE));
        let key = cache_key("host", "vendor/model");

        fetch_context_windows(&provider("good-key"), &dirs).await;
        assert_eq!(cached().get(&key), Some(&window(131_072)));

        // The host now rejects the key: the cached answer stands, and the
        // failure is a warning rather than an empty cache.
        fetch_context_windows(&provider("bad-key"), &dirs).await;
        assert_eq!(cached().get(&key), Some(&window(131_072)));
    }
}
