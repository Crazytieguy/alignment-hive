use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

pub const DEFAULT_CAPTURE_RESPONSE_BODY_BYTES: usize = 10 * 1024 * 1024;
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 100 * 1024 * 1024;

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Listening address. Only loopback addresses are accepted.
    #[serde_inline_default(IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub bind_address: IpAddr,

    /// Listening TCP port.
    #[serde_inline_default(8787)]
    pub port: u16,

    /// Anthropic API base URL.
    #[serde_inline_default("https://api.anthropic.com".to_string())]
    pub anthropic_upstream_base: String,

    /// Named model upstreams. Only `codex` is supported today.
    #[serde_inline_default(default_upstreams())]
    pub upstreams: BTreeMap<String, UpstreamConfig>,

    /// Maximum accepted inbound request-body size in bytes.
    #[serde_inline_default(DEFAULT_MAX_REQUEST_BODY_BYTES)]
    pub max_request_body_bytes: usize,

    /// Ingress token: requests are only accepted under the `/t/<token>/`
    /// path prefix, so other local processes cannot use the routed Codex
    /// credential. When absent, `serve` loads a create-once random token
    /// from the state dir.
    pub ingress_token: Option<String>,

    /// Exact model routing allowlist.
    #[serde_inline_default(default_models())]
    pub models: Vec<ModelRoute>,

    /// Optional request/response capture settings.
    #[serde(default)]
    pub capture: CaptureConfig,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ModelRoute {
    pub routing_id: String,

    #[serde_inline_default("codex".to_string())]
    pub upstream: String,

    pub upstream_model: String,
    pub display_name: String,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct UpstreamConfig {
    #[serde_inline_default(UpstreamMode::Managed)]
    pub mode: UpstreamMode,

    /// Managed mode's loopback child port.
    #[serde_inline_default(8317)]
    pub port: u16,

    /// External mode's loopback base URL.
    pub base_url: Option<String>,

    /// External mode's optional gateway credential.
    pub api_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UpstreamMode {
    #[default]
    Managed,
    External,
    Stub,
}

impl UpstreamMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::External => "external",
            Self::Stub => "stub",
        }
    }
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CaptureConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde_inline_default(PathBuf::from("model-router-capture.jsonl"))]
    pub file: PathBuf,

    /// Maximum response-body bytes retained in each capture record.
    #[serde_inline_default(DEFAULT_CAPTURE_RESPONSE_BODY_BYTES)]
    pub max_response_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str("").expect("every Config field must have a serde default")
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        toml::from_str("").expect("every CaptureConfig field must have a serde default")
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        toml::from_str("").expect("every UpstreamConfig field must have a serde default")
    }
}

fn default_upstreams() -> BTreeMap<String, UpstreamConfig> {
    BTreeMap::from([("codex".to_string(), UpstreamConfig::default())])
}

/// Renders the commented `[[models]]` template section from the actual
/// defaults, so the shipped template can never drift from `default_models()`.
fn template_models_section(models: &[ModelRoute]) -> String {
    models
        .iter()
        .map(|route| {
            format!(
                "#[[models]]\n#routing-id = \"{}\"\n#upstream = \"{}\"\n#upstream-model = \
                 \"{}\"\n#display-name = \"{}\"\n",
                route.routing_id, route.upstream, route.upstream_model, route.display_name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn default_models() -> Vec<ModelRoute> {
    [
        ("claude-gpt-5.6-sol", "gpt-5.6-sol", "GPT-5.6 Sol"),
        ("claude-gpt-5.6-terra", "gpt-5.6-terra", "GPT-5.6 Terra"),
        ("claude-gpt-5.6-luna", "gpt-5.6-luna", "GPT-5.6 Luna"),
        ("gpt-5.6-sol", "gpt-5.6-sol", "GPT-5.6 Sol"),
        ("gpt-5.6-terra", "gpt-5.6-terra", "GPT-5.6 Terra"),
        ("gpt-5.6-luna", "gpt-5.6-luna", "GPT-5.6 Luna"),
    ]
    .into_iter()
    .map(|(routing_id, upstream_model, display_name)| ModelRoute {
        routing_id: routing_id.to_string(),
        upstream: "codex".to_string(),
        upstream_model: upstream_model.to_string(),
        display_name: display_name.to_string(),
    })
    .collect()
}

impl Config {
    /// Loads a TOML config, or returns defaults when the path does not exist.
    ///
    /// # Errors
    /// Returns an error for unreadable, invalid, or unsafe configuration.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let config = if path.exists() {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            toml::from_str(&contents)
                .with_context(|| format!("failed to parse config {}", path.display()))?
        } else {
            tracing::info!(config_path = %path.display(), "Config file not found; using defaults");
            Self::default()
        };
        Self::validate(&config)?;
        Ok(config)
    }

    /// Validates loopback binding, upstream URLs, and routing entries.
    ///
    /// # Errors
    /// Returns an error when any safety or consistency invariant is violated.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.bind_address.is_loopback(),
            "bind-address must be loopback; refusing to bind to {}",
            self.bind_address
        );
        validate_base_url("anthropic-upstream-base", &self.anthropic_upstream_base)?;
        ensure!(
            self.max_request_body_bytes > 0,
            "max-request-body-bytes must be greater than zero"
        );
        if let Some(token) = &self.ingress_token {
            ensure!(!token.is_empty(), "ingress-token cannot be empty");
            ensure!(
                token.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b'~')),
                "ingress-token may only contain URL-safe characters (alphanumeric, -, _, ., ~)"
            );
        }

        for name in self.upstreams.keys() {
            ensure!(
                name == "codex",
                "only the upstream name `codex` is supported today; found {name:?}"
            );
        }
        let codex = self.upstreams.get("codex").ok_or_else(|| {
            anyhow::anyhow!("upstreams must define `codex`; only `codex` is supported today")
        })?;
        validate_codex_upstream(codex)?;

        let mut routing_ids = HashSet::new();
        for route in &self.models {
            ensure!(
                !route.routing_id.is_empty(),
                "model routing-id cannot be empty"
            );
            ensure!(
                !route.upstream_model.is_empty(),
                "upstream-model cannot be empty for {}",
                route.routing_id
            );
            ensure!(
                !route.display_name.is_empty(),
                "display-name cannot be empty for {}",
                route.routing_id
            );
            ensure!(
                route.upstream == "codex",
                "model {} references upstream {:?}; only `codex` is supported today",
                route.routing_id,
                route.upstream
            );
            ensure!(
                routing_ids.insert(&route.routing_id),
                "duplicate model routing-id: {}",
                route.routing_id
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn template() -> String {
        let defaults = Self::default();
        format!(
            r#"# model-router configuration (experimental)
# The router refuses all non-loopback bind addresses.
# Default: {bind_address}
# bind-address = "{bind_address}"

# Default: {port}
# port = {port}

# Claude requests are forwarded here with their original body and credentials.
# Default: "{anthropic_base}"
# anthropic-upstream-base = "{anthropic_base}"

# Named upstreams default to one managed Codex upstream when this table is
# absent. Managed mode is started by the supervisor (implemented separately)
# and binds its child to loopback on the configured port.
#[upstreams.codex]
#mode = "managed"
#port = 8317

# External mode connects to a user-run CLIProxyAPI. The URL MUST use a
# loopback IP literal (127.0.0.0/8 or ::1); hostnames including "localhost"
# are rejected because this is the boundary that receives the injected GPT
# gateway credential.
#[upstreams.codex]
#mode = "external"
#base-url = "http://127.0.0.1:8317"
# Optional local CLIProxyAPI gateway secret. When set, GPT requests receive
# both `x-api-key: <key>` and `Authorization: Bearer <key>` after all incoming
# Claude credentials have been removed. It is never sent to Anthropic.
#api-key = "replace-with-a-local-secret"

# Stub mode uses the built-in protocol smoke-test backend.
#[upstreams.codex]
#mode = "stub"

# Maximum accepted inbound request-body size in bytes. Oversized requests get
# a 413 error instead of being buffered without bound.
# Default: {max_request_body_bytes} (100 MiB)
# max-request-body-bytes = {max_request_body_bytes}

# Ingress token: the router only accepts requests under the /t/<token>/ path
# prefix (ANTHROPIC_BASE_URL includes it), so other local processes cannot
# spend your Codex subscription through the loopback port. When unset,
# `serve` uses a create-once random token stored in the state dir. Set it
# only to pin a known value; URL-safe characters only.
# ingress-token = "replace-with-a-random-token"

# GPT routing is exact-match only. Requests for every other model go to
# Anthropic. By default the six GPT-5.6 routes below are enabled; writing
# any [[models]] entry replaces the whole default list.
{models}
#[capture]
# Capture is off by default. Captures include prompt and response bodies; keep
# the file private. Credential and cookie header values are always redacted.
# Default: false
#enabled = false
# A relative path is resolved against the state directory
# (~/.local/state/model-router by default).
# Default: "{capture_file}"
#file = "{capture_file}"
# Maximum response-body bytes retained per request. Streaming to the client is
# never truncated; capture records mark when this limit was reached.
# Default: {capture_max_response_body_bytes} (10 MiB)
#max-response-body-bytes = {capture_max_response_body_bytes}
"#,
            bind_address = defaults.bind_address,
            port = defaults.port,
            max_request_body_bytes = defaults.max_request_body_bytes,
            anthropic_base = defaults.anthropic_upstream_base,
            models = template_models_section(&defaults.models),
            capture_file = defaults.capture.file.display(),
            capture_max_response_body_bytes = defaults.capture.max_response_body_bytes,
        )
    }
}

fn validate_base_url(name: &str, value: &str) -> anyhow::Result<()> {
    ensure!(
        value.starts_with("http://") || value.starts_with("https://"),
        "{name} must start with http:// or https://"
    );
    ensure!(
        !value.ends_with('?'),
        "{name} cannot end with a query marker"
    );
    Ok(())
}

fn validate_gpt_upstream_base(value: &str) -> anyhow::Result<()> {
    const BOUNDARY: &str = "gpt-upstream-base must use a loopback IP literal \
        (127.0.0.0/8 or ::1); hostnames (including localhost) and non-loopback \
        addresses are rejected because this is the GPT credential injection boundary";

    let url = reqwest::Url::parse(value)
        .with_context(|| format!("invalid gpt-upstream-base URL; {BOUNDARY}"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "gpt-upstream-base must start with http:// or https://; {BOUNDARY}"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("gpt-upstream-base has no host; {BOUNDARY}"))?;
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ip = ip_literal
        .parse::<IpAddr>()
        .map_err(|_| anyhow::anyhow!("gpt-upstream-base host {host:?} is not an IP; {BOUNDARY}"))?;
    ensure!(
        ip.is_loopback(),
        "gpt-upstream-base host {host:?} is not loopback; {BOUNDARY}"
    );
    Ok(())
}

fn validate_codex_upstream(upstream: &UpstreamConfig) -> anyhow::Result<()> {
    match upstream.mode {
        UpstreamMode::External => {
            let base_url = upstream.base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!("[upstreams.codex] base-url is required when mode = \"external\"")
            })?;
            validate_gpt_upstream_base(base_url)?;
            if let Some(api_key) = &upstream.api_key {
                ensure!(
                    !api_key.is_empty(),
                    "[upstreams.codex] api-key cannot be empty"
                );
                crate::headers::GptUpstreamCredential::new(api_key)
                    .context("[upstreams.codex] api-key is not a valid HTTP header value")?;
            }
        }
        mode @ (UpstreamMode::Managed | UpstreamMode::Stub) => {
            let mode = mode.as_str();
            ensure!(
                upstream.base_url.is_none(),
                "[upstreams.codex] base-url must be absent when mode = {mode:?}"
            );
            ensure!(
                upstream.api_key.is_none(),
                "[upstreams.codex] api-key must be absent when mode = {mode:?}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_validate(source: &str) -> Config {
        let config: Config = toml::from_str(source).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.bind_address, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.port, 8787);
        assert_eq!(config.upstreams.len(), 1);
        let codex = &config.upstreams["codex"];
        assert_eq!(codex.mode, UpstreamMode::Managed);
        assert_eq!(codex.port, 8317);
        assert!(codex.base_url.is_none());
        assert!(codex.api_key.is_none());
        assert!(!config.capture.enabled);
        assert_eq!(
            config.capture.max_response_body_bytes,
            DEFAULT_CAPTURE_RESPONSE_BODY_BYTES
        );
        config.validate().unwrap();
    }

    #[test]
    fn absent_upstreams_table_defaults_to_managed_codex() {
        let config = parse_and_validate("port = 9000");
        assert_eq!(config.upstreams.keys().collect::<Vec<_>>(), ["codex"]);
        assert_eq!(config.upstreams["codex"], UpstreamConfig::default());
    }

    #[test]
    fn route_references_default_codex_when_upstream_is_omitted() {
        let config = parse_and_validate(
            r#"
                [[models]]
                routing-id = "claude-gpt-test"
                upstream-model = "gpt-test"
                display-name = "GPT Test"
            "#,
        );
        assert_eq!(config.models[0].upstream, "codex");
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let config: Config = toml::from_str("bind-address = \"0.0.0.0\"").unwrap();
        assert!(Config::validate(&config).is_err());
    }

    #[test]
    fn template_parses_when_example_is_left_commented() {
        let template = Config::template();
        let config: Config = toml::from_str(&template).unwrap();
        assert_eq!(config.port, Config::default().port);
        assert_eq!(config.upstreams["codex"].mode, UpstreamMode::Managed);
        assert_eq!(config.models, Config::default().models);
        assert_eq!(
            config
                .models
                .iter()
                .map(|route| route.routing_id.as_str())
                .collect::<Vec<_>>(),
            [
                "claude-gpt-5.6-sol",
                "claude-gpt-5.6-terra",
                "claude-gpt-5.6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna"
            ]
        );
        for example in [
            "#mode = \"managed\"",
            "#mode = \"external\"",
            "#mode = \"stub\"",
            "#routing-id = \"claude-gpt-5.6-sol\"",
            "#routing-id = \"claude-gpt-5.6-terra\"",
            "#routing-id = \"claude-gpt-5.6-luna\"",
            "#routing-id = \"gpt-5.6-sol\"",
            "#routing-id = \"gpt-5.6-terra\"",
            "#routing-id = \"gpt-5.6-luna\"",
        ] {
            assert!(template.contains(example), "missing {example:?}");
        }
    }

    #[test]
    fn unknown_upstream_name_is_rejected() {
        let config: Config = toml::from_str(
            r#"
                [upstreams.other]
                mode = "stub"
            "#,
        )
        .unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("only the upstream name `codex` is supported today"));
        assert!(error.contains("other"));
    }

    #[test]
    fn route_referencing_unsupported_upstream_is_rejected() {
        let config: Config = toml::from_str(
            r#"
                [upstreams.codex]
                mode = "stub"

                [[models]]
                routing-id = "claude-gpt-test"
                upstream = "other"
                upstream-model = "gpt-test"
                display-name = "GPT Test"
            "#,
        )
        .unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("only `codex` is supported today"));
        assert!(error.contains("other"));
    }

    #[test]
    fn explicit_empty_upstreams_table_is_rejected() {
        let config: Config = toml::from_str("[upstreams]").unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("upstreams must define `codex`"));
    }

    #[test]
    fn managed_and_stub_reject_external_only_fields() {
        for (mode, field) in [
            ("managed", "base-url = \"http://127.0.0.1:8317\""),
            ("managed", "api-key = \"secret\""),
            ("stub", "base-url = \"http://127.0.0.1:8317\""),
            ("stub", "api-key = \"secret\""),
        ] {
            let config: Config =
                toml::from_str(&format!("[upstreams.codex]\nmode = {mode:?}\n{field}")).unwrap();
            let error = config.validate().unwrap_err().to_string();
            let field_name = field.split_once(' ').unwrap().0;
            assert!(error.contains(field_name), "{error}");
            assert!(error.contains(&format!("mode = {mode:?}")), "{error}");
        }
    }

    #[test]
    fn external_requires_base_url() {
        let config: Config = toml::from_str(
            r#"
                [upstreams.codex]
                mode = "external"
            "#,
        )
        .unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("base-url is required when mode = \"external\""));
    }

    #[test]
    fn external_accepts_loopback_ip_literals_and_optional_credential() {
        for base in [
            "http://127.0.0.1:8317",
            "http://127.42.0.9:8317",
            "http://[::1]:8317",
        ] {
            let config = parse_and_validate(&format!(
                r#"
                    [upstreams.codex]
                    mode = "external"
                    base-url = {base:?}
                    api-key = "local-gateway-secret"
                "#
            ));
            assert_eq!(config.upstreams["codex"].base_url.as_deref(), Some(base));
            assert_eq!(
                config.upstreams["codex"].api_key.as_deref(),
                Some("local-gateway-secret")
            );
        }
    }

    #[test]
    fn external_rejects_empty_or_invalid_api_key() {
        for api_key in ["", "line\nfeed"] {
            let config: Config = toml::from_str(&format!(
                r#"
                    [upstreams.codex]
                    mode = "external"
                    base-url = "http://127.0.0.1:8317"
                    api-key = {api_key:?}
                "#
            ))
            .unwrap();
            let error = format!("{:#}", config.validate().unwrap_err());
            assert!(error.contains("api-key"), "{error}");
        }
    }

    #[test]
    fn external_rejects_hostnames_and_non_loopback_ips() {
        for base in [
            "http://localhost:8317",
            "http://cliproxy.internal:8317",
            "http://10.0.0.2:8317",
            "http://[2001:db8::1]:8317",
        ] {
            let config: Config = toml::from_str(&format!(
                r#"
                    [upstreams.codex]
                    mode = "external"
                    base-url = {base:?}
                "#
            ))
            .unwrap();
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains("loopback IP literal"), "{error}");
            assert!(error.contains("credential injection boundary"), "{error}");
        }
    }

    #[test]
    fn removed_flat_fields_and_unknown_nested_fields_are_rejected() {
        for source in [
            "gpt-upstream-base = \"stub\"",
            "gpt-upstream-api-key = \"secret\"",
            "[upstreams.codex]\nmode = \"stub\"\nextra = true",
            "[[models]]\nrouting-id = \"route\"\nupstream-model = \"model\"\ndisplay-name = \"Model\"\nextra = true",
        ] {
            assert!(
                toml::from_str::<Config>(source).is_err(),
                "accepted {source:?}"
            );
        }
    }

    #[test]
    fn max_request_body_bytes_must_be_positive() {
        let config: Config = toml::from_str("max-request-body-bytes = 0").unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("max-request-body-bytes must be greater than zero")
        );
    }
}
