//! `model-router verify-providers`: checks each configured
//! `[[openai-providers]]` entry against the provider's authenticated
//! `GET /models` endpoint, reporting which configured model IDs exist.
//!
//! Exists so setup flows never have to put the API key in a command line or
//! read the populated config file: the key stays inside this process and is
//! scrubbed from any echoed error output.

use anyhow::Context;
use serde::Serialize;

use crate::config::{Config, OpenAiProvider};

#[derive(Debug, Serialize)]
pub struct ProviderReport {
    pub provider: String,
    pub base_url: String,
    pub ok: bool,
    pub detail: String,
    pub models: Vec<ModelCheck>,
}

#[derive(Debug, Serialize)]
pub struct ModelCheck {
    pub name: String,
    pub routing_id: String,
    pub found: bool,
    /// The window this host actually guarantees, when it reports one. Hosts
    /// vary: many OpenAI-compatible catalogs omit the field entirely.
    pub host_context_length: Option<u64>,
    /// The catalog's headline number, when it is larger than the guaranteed
    /// one — i.e. when sub-providers disagree.
    pub advertised_context_length: Option<u64>,
    /// The `context-window` configured for this model, when set.
    pub configured_context_window: Option<u64>,
}

impl ModelCheck {
    /// `(configured, host)` when the configured window claims more than the
    /// host serves — the direction that licences requests the host rejects.
    /// A host that reports no window cannot contradict anything.
    fn oversized(&self) -> Option<(u64, u64)> {
        let (configured, host) = (self.configured_context_window?, self.host_context_length?);
        (configured > host).then_some((configured, host))
    }
}

/// Verifies all configured providers (or just `name`).
///
/// # Errors
/// Returns an error when the config cannot be loaded, no providers are
/// configured, or `name` does not match a configured provider. Per-provider
/// network/auth failures are reported in the result, not as errors.
pub async fn run(
    config_path: &std::path::Path,
    name: Option<&str>,
) -> anyhow::Result<Vec<ProviderReport>> {
    let config = Config::load(config_path)?;
    anyhow::ensure!(
        !config.openai_providers.is_empty(),
        "no [[openai-providers]] configured in {}",
        config_path.display()
    );
    let selected: Vec<&OpenAiProvider> = match name {
        Some(name) => {
            let provider = config
                .openai_providers
                .iter()
                .find(|provider| provider.name == name)
                .with_context(|| format!("no openai-provider named {name:?} in the config"))?;
            vec![provider]
        }
        None => config.openai_providers.iter().collect(),
    };
    // No redirects: reqwest strips Authorization on cross-host hops anyway,
    // which would surface as a baffling 401 — fail loudly instead.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut reports = Vec::new();
    for provider in selected {
        reports.push(verify_one(&client, provider).await);
    }
    Ok(reports)
}

async fn verify_one(client: &reqwest::Client, provider: &OpenAiProvider) -> ProviderReport {
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let mut report = ProviderReport {
        provider: provider.name.clone(),
        base_url: provider.base_url.clone(),
        ok: false,
        detail: String::new(),
        models: Vec::new(),
    };
    let Some(api_key) = provider.api_key.as_deref() else {
        report.detail = format!(
            "no api-key: add `{} = \"<key>\"` under [openai-providers] in \
             ~/.config/model-router/secrets.toml (chmod 600)",
            provider.name
        );
        return report;
    };
    let response = match client.get(&url).bearer_auth(api_key).send().await {
        Ok(response) => response,
        Err(error) => {
            // reqwest errors never echo request headers, but scrub anyway:
            // this string is the only place a mistake could leak the key.
            report.detail = scrub(&format!("request failed: {error:#}"), api_key);
            return report;
        }
    };
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<none>");
        report.detail = scrub(
            &format!("endpoint redirected ({status}) to {location}; use the final base-url"),
            api_key,
        );
        return report;
    }
    let body = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = String::from_utf8_lossy(&body).chars().take(300).collect();
        report.detail = scrub(&format!("HTTP {status}: {snippet}"), api_key);
        return report;
    }
    let Some(catalog) = parse_catalog(&body) else {
        report.detail = "HTTP 200 but the response has no data[].id list".to_string();
        return report;
    };
    let base = provider.base_url.trim_end_matches('/');
    for model in &provider.models {
        let entry = catalog.iter().find(|entry| entry.id == model.name);
        let advertised = entry.and_then(|entry| entry.context_length);
        let guaranteed = match advertised {
            Some(advertised) => Some(
                crate::discovery::host_window(client, base, api_key, &model.name, advertised).await,
            ),
            None => None,
        };
        report.models.push(ModelCheck {
            found: entry.is_some(),
            host_context_length: guaranteed,
            advertised_context_length: advertised
                .filter(|advertised| Some(*advertised) != guaranteed),
            configured_context_window: model.context_window,
            name: model.name.clone(),
            routing_id: model.routing_id.clone(),
        });
    }
    let missing = report.models.iter().filter(|check| !check.found).count();
    let oversized = report
        .models
        .iter()
        .filter(|check| check.oversized().is_some())
        .count();
    report.ok = missing == 0 && oversized == 0;
    report.detail = if missing > 0 {
        format!(
            "{missing} of {} configured models not in the provider's /models list",
            report.models.len()
        )
    } else if oversized == 0 {
        format!("all {} configured models found", report.models.len())
    } else {
        format!(
            "all {} configured models found, but {oversized} configured context-window(s) \
             exceed the host's own limit",
            report.models.len()
        )
    };
    report
}

/// One entry of a provider's `/models` catalog.
pub(crate) struct CatalogEntry {
    pub(crate) id: String,
    pub(crate) context_length: Option<u64>,
}

pub(crate) fn parse_catalog(body: &[u8]) -> Option<Vec<CatalogEntry>> {
    /// Spellings seen across OpenAI-compatible catalogs, most common first.
    const CONTEXT_KEYS: [&str; 3] = ["context_length", "context_window", "max_context_length"];

    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(
        value
            .get("data")?
            .as_array()?
            .iter()
            .filter_map(|model| {
                let id = model.get("id").and_then(serde_json::Value::as_str)?;
                Some(CatalogEntry {
                    id: id.to_owned(),
                    context_length: CONTEXT_KEYS
                        .iter()
                        .find_map(|key| model.get(key).and_then(serde_json::Value::as_u64)),
                })
            })
            .collect(),
    )
}

fn scrub(text: &str, api_key: &str) -> String {
    text.replace(api_key, "[redacted]")
}

/// Renders an error without echoing the key. reqwest never includes request
/// headers in its Display, but this is the one place a mistake would leak.
pub(crate) fn scrub_key(error: &impl std::fmt::Display, api_key: &str) -> String {
    scrub(&format!("{error}"), api_key)
}

/// Renders human-readable output; returns whether every provider verified.
#[must_use]
pub fn render(reports: &[ProviderReport]) -> (String, bool) {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut all_ok = true;
    for report in reports {
        all_ok &= report.ok;
        let marker = if report.ok { "ok" } else { "FAIL" };
        let _ = writeln!(
            out,
            "[{marker}] {} ({}): {}",
            report.provider, report.base_url, report.detail
        );
        for check in &report.models {
            let context = match (check.oversized(), check.host_context_length) {
                (Some((configured, host)), _) => {
                    format!("  (host context {host}, configured {configured} — TOO LARGE)")
                }
                (None, Some(host)) => match check.advertised_context_length {
                    // The catalog headline is only reachable once the account
                    // restricts routing to the sub-providers that serve it.
                    Some(advertised) => format!(
                        "  (host context {host} guaranteed, {advertised} advertised — varies by                          sub-provider)"
                    ),
                    None => format!("  (host context {host})"),
                },
                (None, None) => String::new(),
            };
            let _ = writeln!(
                out,
                "       {} {} -> {}{context}",
                if check.found { "found  " } else { "missing" },
                check.name,
                check.routing_id
            );
        }
    }
    (out, all_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_the_key() {
        assert_eq!(
            scrub("bad key fw-123 rejected", "fw-123"),
            "bad key [redacted] rejected"
        );
    }

    #[test]
    fn parse_catalog_reads_openai_shape() {
        let body = br#"{"object":"list","data":[{"id":"a"},{"id":"b"},{"no_id":true}]}"#;
        let catalog = parse_catalog(body).unwrap();
        assert_eq!(
            catalog
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(catalog.iter().all(|entry| entry.context_length.is_none()));
        assert!(parse_catalog(b"[]").is_none());
        assert!(parse_catalog(b"not json").is_none());
    }

    #[test]
    fn parse_catalog_reads_the_host_context_length() {
        let body = br#"{"data":[{"id":"a","context_length":1048576},{"id":"b","context_window":128000},{"id":"c"}]}"#;
        let catalog = parse_catalog(body).unwrap();
        assert_eq!(catalog[0].context_length, Some(1_048_576));
        assert_eq!(catalog[1].context_length, Some(128_000));
        assert_eq!(catalog[2].context_length, None);
    }

    fn check(host: Option<u64>, configured: Option<u64>) -> ModelCheck {
        ModelCheck {
            name: "m".to_string(),
            routing_id: "m".to_string(),
            found: true,
            host_context_length: host,
            advertised_context_length: None,
            configured_context_window: configured,
        }
    }

    #[test]
    fn only_a_window_the_host_contradicts_counts_as_oversized() {
        assert_eq!(
            check(Some(256_000), Some(1_000_000)).oversized(),
            Some((1_000_000, 256_000))
        );
        assert!(check(Some(1_000_000), Some(256_000)).oversized().is_none());
        assert!(check(Some(256_000), Some(256_000)).oversized().is_none());
        // A host that reports nothing cannot contradict the configuration.
        assert!(check(None, Some(1_000_000)).oversized().is_none());
    }
}
