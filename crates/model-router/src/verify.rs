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
    let Some(ids) = parse_model_ids(&body) else {
        report.detail = "HTTP 200 but the response has no data[].id list".to_string();
        return report;
    };
    for model in &provider.models {
        report.models.push(ModelCheck {
            found: ids.iter().any(|id| id == &model.name),
            name: model.name.clone(),
            routing_id: model.routing_id.clone(),
        });
    }
    let missing = report.models.iter().filter(|check| !check.found).count();
    report.ok = missing == 0;
    report.detail = if missing == 0 {
        format!("all {} configured models found", report.models.len())
    } else {
        format!(
            "{missing} of {} configured models not in the provider's /models list",
            report.models.len()
        )
    };
    report
}

fn parse_model_ids(body: &[u8]) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(
        value
            .get("data")?
            .as_array()?
            .iter()
            .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn scrub(text: &str, api_key: &str) -> String {
    text.replace(api_key, "[redacted]")
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
            let _ = writeln!(
                out,
                "       {} {} -> {}",
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
    fn parse_model_ids_reads_openai_shape() {
        let body = br#"{"object":"list","data":[{"id":"a"},{"id":"b"},{"no_id":true}]}"#;
        assert_eq!(parse_model_ids(body).unwrap(), ["a", "b"]);
        assert!(parse_model_ids(b"[]").is_none());
        assert!(parse_model_ids(b"not json").is_none());
    }
}
