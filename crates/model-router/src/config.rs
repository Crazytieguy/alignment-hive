use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

pub const DEFAULT_CAPTURE_RESPONSE_BODY_BYTES: usize = 10 * 1024 * 1024;
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 100 * 1024 * 1024;

/// The single supported upstream name: the managed/external `CLIProxyAPI`
/// gateway. Renamed from `codex` in 0.1.3 (the upstream carries more than
/// Codex traffic now); `codex` is accepted as a deprecated alias at load.
pub const CLIPROXY_UPSTREAM: &str = "cliproxy";
const LEGACY_UPSTREAM: &str = "codex";

/// Namespace prefix for the internal `CLIProxyAPI` model aliases derived from
/// `[[openai-providers]]` entries. Guarantees derived aliases can never
/// collide with native upstream model IDs; never user-visible. No `/` — that
/// character has provider-prefix semantics in `CLIProxyAPI` model IDs.
const DERIVED_ALIAS_PREFIX: &str = "openai-compat--";

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

    /// Named model upstreams. Only `cliproxy` is supported today (`codex` is
    /// the accepted deprecated alias).
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

    /// OpenAI-compatible providers exposed through the managed `CLIProxyAPI`
    /// child (Fireworks, Together, ...). Empty by default.
    #[serde(default, rename = "openai-providers")]
    pub openai_providers: Vec<OpenAiProvider>,

    /// Optional request/response capture settings.
    #[serde(default)]
    pub capture: CaptureConfig,

    /// `WebSearch` sub-call handling for routed GPT models.
    #[serde(default)]
    pub web_search: WebSearchConfig,

    /// Routes derived from `openai_providers` model entries; rebuilt by
    /// [`Config::prepare`], never read from TOML.
    #[serde(skip)]
    pub derived_models: Vec<ModelRoute>,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WebSearchConfig {
    #[serde_inline_default(WebSearchMode::Alpha)]
    pub mode: WebSearchMode,
}

/// How the router answers Claude Code's `WebSearch` sub-call on the GPT branch.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WebSearchMode {
    /// Answer from the Codex search backend (`/v1/alpha/search`), falling
    /// back to `scrape` when that call fails.
    #[default]
    Alpha,
    /// Forward to the LLM upstream as-is, then scrape links from the response
    /// text into empty `web_search_tool_result` blocks.
    Scrape,
    /// Pass `WebSearch` sub-calls through untouched.
    Off,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OpenAiProvider {
    pub name: String,
    pub base_url: String,
    pub models: Vec<ProviderModel>,

    /// Loaded from the sibling `secrets.toml`, never from the config file
    /// (`deny_unknown_fields` rejects an inline `api-key`). `None` means the
    /// provider is configured but keyless: it is skipped in the generated
    /// child config and flagged by doctor and `verify-providers`.
    #[serde(skip)]
    pub api_key: Option<String>,
}

/// `secrets.toml`, sibling of the config file: keeps API keys out of the
/// freely-editable config. `[openai-providers]` maps provider name to key.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SecretsFile {
    #[serde(default)]
    openai_providers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProviderModel {
    /// Exact model ID on the provider (e.g. `accounts/fireworks/models/...`).
    pub name: String,
    /// The model ID Claude Code requests (becomes a routed model).
    pub routing_id: String,
    pub display_name: String,
}

/// The internal `CLIProxyAPI` alias for a provider model. Routing IDs are
/// globally unique and charset-restricted (validation), so prefixing alone is
/// injective; the provider name would add nothing but ambiguity.
#[must_use]
pub fn derived_alias(routing_id: &str) -> String {
    format!("{DERIVED_ALIAS_PREFIX}{routing_id}")
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ModelRoute {
    pub routing_id: String,

    #[serde_inline_default(CLIPROXY_UPSTREAM.to_string())]
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

impl Default for WebSearchConfig {
    fn default() -> Self {
        toml::from_str("").expect("every WebSearchConfig field must have a serde default")
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        toml::from_str("").expect("every UpstreamConfig field must have a serde default")
    }
}

fn default_upstreams() -> BTreeMap<String, UpstreamConfig> {
    BTreeMap::from([(CLIPROXY_UPSTREAM.to_string(), UpstreamConfig::default())])
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
        upstream: CLIPROXY_UPSTREAM.to_string(),
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
        let mut config = if path.exists() {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            parse_toml_sanitized(&contents, path)?
        } else {
            tracing::info!(config_path = %path.display(), "Config file not found; using defaults");
            Self::default()
        };
        config.load_secrets(&path.with_file_name("secrets.toml"))?;
        config.prepare()?;
        Ok(config)
    }

    /// Attaches API keys from `secrets.toml` to matching providers. A missing
    /// file or missing entry is not an error (the provider runs degraded and
    /// doctor/verify-providers point at it); an unreadable or invalid file is.
    ///
    /// # Errors
    /// Returns an error when the secrets file exists but cannot be read or
    /// parsed.
    pub fn load_secrets(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.openai_providers.is_empty() || !path.exists() {
            return Ok(());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read secrets {}", path.display()))?;
        let secrets: SecretsFile = parse_toml_sanitized(&contents, path)?;
        for name in secrets.openai_providers.keys() {
            if !self.openai_providers.iter().any(|p| &p.name == name) {
                tracing::warn!(
                    "secrets.toml names openai-provider {name:?} which is not in the config"
                );
            }
        }
        for provider in &mut self.openai_providers {
            provider.api_key = secrets.openai_providers.get(&provider.name).cloned();
        }
        Ok(())
    }

    /// Normalizes legacy names, rebuilds derived routes, and validates.
    /// Idempotent; every construction path (load, tests, embedding) must call
    /// this before the config is used for routing.
    ///
    /// # Errors
    /// Returns an error when any safety or consistency invariant is violated.
    pub fn prepare(&mut self) -> anyhow::Result<()> {
        self.normalize_legacy_upstream_name()?;
        self.rebuild_derived_models();
        self.validate()
    }

    /// The `cliproxy` upstream (present in every prepared config).
    ///
    /// # Panics
    /// Panics when called on a config that failed or skipped [`Self::prepare`].
    #[must_use]
    pub fn cliproxy_upstream(&self) -> &UpstreamConfig {
        self.upstreams
            .get(CLIPROXY_UPSTREAM)
            .expect("prepared config always contains the cliproxy upstream")
    }

    /// Every route the router serves: configured `[[models]]` plus routes
    /// derived from `[[openai-providers]]`.
    pub fn effective_models(&self) -> impl Iterator<Item = &ModelRoute> {
        self.models.iter().chain(self.derived_models.iter())
    }

    fn normalize_legacy_upstream_name(&mut self) -> anyhow::Result<()> {
        if let Some(legacy) = self.upstreams.remove(LEGACY_UPSTREAM) {
            ensure!(
                !self.upstreams.contains_key(CLIPROXY_UPSTREAM),
                "config defines both [upstreams.{LEGACY_UPSTREAM}] and \
                 [upstreams.{CLIPROXY_UPSTREAM}]; keep only `{CLIPROXY_UPSTREAM}`"
            );
            tracing::warn!(
                "[upstreams.{LEGACY_UPSTREAM}] is deprecated; rename it to \
                 [upstreams.{CLIPROXY_UPSTREAM}]"
            );
            self.upstreams.insert(CLIPROXY_UPSTREAM.to_string(), legacy);
        }
        let mut warned = false;
        for route in &mut self.models {
            if route.upstream == LEGACY_UPSTREAM {
                if !warned {
                    tracing::warn!(
                        "model routes with upstream = \"{LEGACY_UPSTREAM}\" are deprecated; \
                         rename to \"{CLIPROXY_UPSTREAM}\""
                    );
                    warned = true;
                }
                route.upstream = CLIPROXY_UPSTREAM.to_string();
            }
        }
        Ok(())
    }

    fn rebuild_derived_models(&mut self) {
        self.derived_models = self
            .openai_providers
            .iter()
            .flat_map(|provider| {
                provider.models.iter().map(|model| ModelRoute {
                    routing_id: model.routing_id.clone(),
                    upstream: CLIPROXY_UPSTREAM.to_string(),
                    upstream_model: derived_alias(&model.routing_id),
                    display_name: model.display_name.clone(),
                })
            })
            .collect();
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
                name == CLIPROXY_UPSTREAM,
                "only the upstream name `{CLIPROXY_UPSTREAM}` is supported today; found {name:?}"
            );
        }
        let cliproxy = self.upstreams.get(CLIPROXY_UPSTREAM).ok_or_else(|| {
            anyhow::anyhow!(
                "upstreams must define `{CLIPROXY_UPSTREAM}`; only `{CLIPROXY_UPSTREAM}` is \
                 supported today"
            )
        })?;
        validate_cliproxy_upstream(cliproxy)?;

        let mut routing_ids = HashSet::new();
        for route in self.effective_models() {
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
                route.upstream == CLIPROXY_UPSTREAM,
                "model {} references upstream {:?}; only `{CLIPROXY_UPSTREAM}` is supported today",
                route.routing_id,
                route.upstream
            );
            ensure!(
                routing_ids.insert(&route.routing_id),
                "duplicate model routing-id: {}",
                route.routing_id
            );
        }

        self.validate_openai_providers(cliproxy)
    }

    fn validate_openai_providers(&self, cliproxy: &UpstreamConfig) -> anyhow::Result<()> {
        if self.openai_providers.is_empty() {
            return Ok(());
        }
        ensure!(
            cliproxy.mode != UpstreamMode::External,
            "[[openai-providers]] requires [upstreams.{CLIPROXY_UPSTREAM}] mode = \"managed\" \
             (external CLIProxyAPI instances own their provider config; add the \
             openai-compatibility section there instead)"
        );
        let mut names = HashSet::new();
        for provider in &self.openai_providers {
            ensure!(
                !provider.name.is_empty(),
                "openai-provider name cannot be empty"
            );
            ensure!(
                provider
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
                "openai-provider name {:?} may only contain alphanumerics, -, _, .",
                provider.name
            );
            ensure!(
                names.insert(&provider.name),
                "duplicate openai-provider name: {}",
                provider.name
            );
            validate_provider_base_url(&provider.name, &provider.base_url)?;
            if let Some(api_key) = &provider.api_key {
                ensure!(
                    !api_key.is_empty(),
                    "openai-provider {} has an empty api-key in secrets.toml",
                    provider.name
                );
            }
            ensure!(
                !provider.models.is_empty(),
                "openai-provider {} defines no models",
                provider.name
            );
            for model in &provider.models {
                ensure!(
                    !model.name.is_empty(),
                    "openai-provider {} has a model with an empty name",
                    provider.name
                );
                ensure!(
                    !model.routing_id.is_empty()
                        && model.routing_id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        }),
                    "openai-provider {} model {} routing-id {:?} must be non-empty and contain \
                     only alphanumerics, -, _, . (it becomes a CLIProxyAPI model alias)",
                    provider.name,
                    model.name,
                    model.routing_id
                );
                ensure!(
                    !model.display_name.is_empty(),
                    "openai-provider {} model {} has an empty display-name",
                    provider.name,
                    model.name
                );
                ensure!(
                    !model
                        .display_name
                        .bytes()
                        .any(|byte| byte.is_ascii_control()),
                    "openai-provider {} model {} display-name contains control characters",
                    provider.name,
                    model.name
                );
            }
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

# Named upstreams default to one managed CLIProxyAPI upstream when this table
# is absent. Managed mode is started by the supervisor and binds its child to
# loopback on the configured port. (`[upstreams.codex]` is the deprecated
# pre-0.2 name for the same upstream and still loads, with a warning.)
#[upstreams.cliproxy]
#mode = "managed"
#port = 8317

# External mode connects to a user-run CLIProxyAPI. The URL MUST use a
# loopback IP literal (127.0.0.0/8 or ::1); hostnames including "localhost"
# are rejected because this is the boundary that receives the injected GPT
# gateway credential.
#[upstreams.cliproxy]
#mode = "external"
#base-url = "http://127.0.0.1:8317"
# Optional local CLIProxyAPI gateway secret. When set, GPT requests receive
# both `x-api-key: <key>` and `Authorization: Bearer <key>` after all incoming
# Claude credentials have been removed. It is never sent to Anthropic.
#api-key = "replace-with-a-local-secret"

# Stub mode uses the built-in protocol smoke-test backend.
#[upstreams.cliproxy]
#mode = "stub"

# OpenAI-compatible providers (managed mode only): the managed CLIProxyAPI
# child gains these as `openai-compatibility` upstreams, and each model entry
# below becomes a routed model automatically — no [[models]] entry needed.
# `name` is the provider's exact model ID; `routing-id` is what Claude Code
# requests. API keys do NOT go in this file: put them in secrets.toml next
# to it (chmod 600), keyed by provider name:
#   [openai-providers]
#   openrouter = "sk-or-..."
#[[openai-providers]]
#name = "openrouter"
#base-url = "https://openrouter.ai/api/v1"
#
#[[openai-providers.models]]
#name = "moonshotai/kimi-k2.7"
#routing-id = "kimi-k2.7"
#display-name = "Kimi K2.7"

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
# Claude Code implements its WebSearch tool as a side call that runs the
# server-side web_search tool on the session model. The router matches the
# search backend to the agent that asked: GPT-origin searches are answered
# from the Codex search backend (/v1/alpha/search) in a few seconds with
# structured links, Claude-origin searches stay on Anthropic — that is
# "alpha", the default. "scrape" answers GPT-origin searches through the LLM
# upstream and recovers links from the response text (also the automatic
# fallback when the alpha call fails). "off" disables all of it (GPT-session
# searches become slow and Claude Code shows "No links found.").
#[web-search]
# Default: "alpha"
#mode = "alpha"

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

/// Never surfaces the raw toml error: its Display quotes the offending
/// source line, which can contain an api-key (e.g. an unterminated string
/// while pasting one). Reports location + message only.
fn parse_toml_sanitized<T: serde::de::DeserializeOwned>(
    contents: &str,
    path: &Path,
) -> anyhow::Result<T> {
    toml::from_str(contents).map_err(|error| {
        let location = error
            .span()
            .map(|span| {
                let prefix = &contents[..span.start.min(contents.len())];
                let line = prefix.matches('\n').count() + 1;
                let column = prefix.rsplit('\n').next().map_or(0, str::len) + 1;
                format!(" at line {line}, column {column}")
            })
            .unwrap_or_default();
        anyhow::anyhow!(
            "failed to parse {}{location}: {}",
            path.display(),
            error.message()
        )
    })
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

fn validate_cliproxy_upstream(upstream: &UpstreamConfig) -> anyhow::Result<()> {
    match upstream.mode {
        UpstreamMode::External => {
            let base_url = upstream.base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "[upstreams.{CLIPROXY_UPSTREAM}] base-url is required when mode = \"external\""
                )
            })?;
            validate_gpt_upstream_base(base_url)?;
            if let Some(api_key) = &upstream.api_key {
                ensure!(
                    !api_key.is_empty(),
                    "[upstreams.{CLIPROXY_UPSTREAM}] api-key cannot be empty"
                );
                crate::headers::GptUpstreamCredential::new(api_key).with_context(|| {
                    format!(
                        "[upstreams.{CLIPROXY_UPSTREAM}] api-key is not a valid HTTP header value"
                    )
                })?;
            }
        }
        mode @ (UpstreamMode::Managed | UpstreamMode::Stub) => {
            let mode = mode.as_str();
            ensure!(
                upstream.base_url.is_none(),
                "[upstreams.{CLIPROXY_UPSTREAM}] base-url must be absent when mode = {mode:?}"
            );
            ensure!(
                upstream.api_key.is_none(),
                "[upstreams.{CLIPROXY_UPSTREAM}] api-key must be absent when mode = {mode:?}"
            );
        }
    }
    Ok(())
}

fn validate_provider_base_url(provider: &str, value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(value)
        .with_context(|| format!("openai-provider {provider} base-url is not a valid URL"))?;
    ensure!(
        url.scheme() == "https",
        "openai-provider {provider} base-url must use https (remote host receiving your \
         provider API key); found {value:?}"
    );
    ensure!(
        url.host_str().is_some(),
        "openai-provider {provider} base-url has no host"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_prepare(source: &str) -> Config {
        let mut config: Config = toml::from_str(source).unwrap();
        config.prepare().unwrap();
        config
    }

    fn prepare_error(source: &str) -> String {
        let mut config: Config = toml::from_str(source).unwrap();
        format!("{:#}", config.prepare().unwrap_err())
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.bind_address, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.port, 8787);
        assert_eq!(config.upstreams.len(), 1);
        let cliproxy = &config.upstreams["cliproxy"];
        assert_eq!(cliproxy.mode, UpstreamMode::Managed);
        assert_eq!(cliproxy.port, 8317);
        assert!(cliproxy.base_url.is_none());
        assert!(cliproxy.api_key.is_none());
        assert!(!config.capture.enabled);
        assert_eq!(
            config.capture.max_response_body_bytes,
            DEFAULT_CAPTURE_RESPONSE_BODY_BYTES
        );
        config.validate().unwrap();
    }

    #[test]
    fn absent_upstreams_table_defaults_to_managed_cliproxy() {
        let config = parse_and_prepare("port = 9000");
        assert_eq!(config.upstreams.keys().collect::<Vec<_>>(), ["cliproxy"]);
        assert_eq!(config.upstreams["cliproxy"], UpstreamConfig::default());
    }

    #[test]
    fn route_references_default_cliproxy_when_upstream_is_omitted() {
        let config = parse_and_prepare(
            r#"
                [[models]]
                routing-id = "claude-gpt-test"
                upstream-model = "gpt-test"
                display-name = "GPT Test"
            "#,
        );
        assert_eq!(config.models[0].upstream, "cliproxy");
    }

    #[test]
    fn web_search_defaults_to_alpha_and_parses_every_mode() {
        assert_eq!(Config::default().web_search.mode, WebSearchMode::Alpha);
        for (source, mode) in [
            ("alpha", WebSearchMode::Alpha),
            ("scrape", WebSearchMode::Scrape),
            ("off", WebSearchMode::Off),
        ] {
            let config = parse_and_prepare(&format!("[web-search]\nmode = \"{source}\""));
            assert_eq!(config.web_search.mode, mode);
        }
        assert!(toml::from_str::<Config>("[web-search]\nmode = \"bogus\"").is_err());
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
        assert_eq!(config.upstreams["cliproxy"].mode, UpstreamMode::Managed);
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
        assert!(error.contains("only the upstream name `cliproxy` is supported today"));
        assert!(error.contains("other"));
    }

    #[test]
    fn route_referencing_unsupported_upstream_is_rejected() {
        let config: Config = toml::from_str(
            r#"
                [upstreams.cliproxy]
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
        assert!(error.contains("only `cliproxy` is supported today"));
        assert!(error.contains("other"));
    }

    #[test]
    fn explicit_empty_upstreams_table_is_rejected() {
        let error = prepare_error("[upstreams]");
        assert!(error.contains("upstreams must define `cliproxy`"));
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
                toml::from_str(&format!("[upstreams.cliproxy]\nmode = {mode:?}\n{field}")).unwrap();
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
                [upstreams.cliproxy]
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
            let config = parse_and_prepare(&format!(
                r#"
                    [upstreams.cliproxy]
                    mode = "external"
                    base-url = {base:?}
                    api-key = "local-gateway-secret"
                "#
            ));
            assert_eq!(config.upstreams["cliproxy"].base_url.as_deref(), Some(base));
            assert_eq!(
                config.upstreams["cliproxy"].api_key.as_deref(),
                Some("local-gateway-secret")
            );
        }
    }

    #[test]
    fn external_rejects_empty_or_invalid_api_key() {
        for api_key in ["", "line\nfeed"] {
            let config: Config = toml::from_str(&format!(
                r#"
                    [upstreams.cliproxy]
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
                    [upstreams.cliproxy]
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
            "[upstreams.cliproxy]\nmode = \"stub\"\nextra = true",
            "[[models]]\nrouting-id = \"route\"\nupstream-model = \"model\"\ndisplay-name = \"Model\"\nextra = true",
        ] {
            assert!(
                toml::from_str::<Config>(source).is_err(),
                "accepted {source:?}"
            );
        }
    }

    #[test]
    fn parse_errors_never_echo_config_source() {
        // An unterminated api-key string is the realistic slip while pasting
        // a secret; the raw toml error would quote the whole source line.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[openai-providers]]\nname = \"p\"\napi-key = \"sk-SENTINEL-DO-NOT-PRINT\n",
        )
        .unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(!error.contains("SENTINEL"), "{error}");
        assert!(error.contains("failed to parse"), "{error}");
        assert!(error.contains("line 3"), "{error}");

        // The same sanitization covers secrets.toml (where keys actually live).
        std::fs::write(
            path.with_file_name("secrets.toml"),
            "[openai-providers]\np = \"sk-SENTINEL\n",
        )
        .unwrap();
        std::fs::write(&path, provider_toml(KIMI_MODEL)).unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(!error.contains("SENTINEL"), "{error}");
        assert!(error.contains("secrets.toml"), "{error}");
    }

    #[test]
    fn secrets_file_attaches_keys_by_provider_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, provider_toml(KIMI_MODEL)).unwrap();
        // No secrets file: provider loads keyless.
        assert_eq!(
            Config::load(&path).unwrap().openai_providers[0].api_key,
            None
        );
        // Matching entry attaches; unmatched names only warn.
        std::fs::write(
            path.with_file_name("secrets.toml"),
            "[openai-providers]\nfireworks = \"fw-live\"\nghost = \"unused\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.openai_providers[0].api_key.as_deref(),
            Some("fw-live")
        );
    }

    #[test]
    fn legacy_codex_upstream_and_routes_normalize_via_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                [upstreams.codex]
                mode = "stub"

                [[models]]
                routing-id = "claude-gpt-test"
                upstream = "codex"
                upstream-model = "gpt-test"
                display-name = "GPT Test"
            "#,
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.upstreams.keys().collect::<Vec<_>>(), ["cliproxy"]);
        assert_eq!(config.upstreams["cliproxy"].mode, UpstreamMode::Stub);
        assert_eq!(config.models[0].upstream, "cliproxy");
        assert_eq!(config.cliproxy_upstream().mode, UpstreamMode::Stub);
    }

    #[test]
    fn defining_both_legacy_and_new_upstream_keys_is_rejected() {
        let error = prepare_error(
            r#"
                [upstreams.codex]
                mode = "stub"
                [upstreams.cliproxy]
                mode = "managed"
            "#,
        );
        assert!(error.contains("both"), "{error}");
        assert!(error.contains("keep only `cliproxy`"), "{error}");
    }

    fn provider_toml(models: &str) -> String {
        format!(
            r#"
                [[openai-providers]]
                name = "fireworks"
                base-url = "https://api.fireworks.ai/inference/v1"
                {models}
            "#
        )
    }

    const KIMI_MODEL: &str = r#"
        [[openai-providers.models]]
        name = "accounts/fireworks/models/kimi-k2p7"
        routing-id = "kimi-k2.7"
        display-name = "Kimi K2.7"
    "#;

    #[test]
    fn provider_models_become_derived_routes_with_namespaced_aliases() {
        let config = parse_and_prepare(&provider_toml(KIMI_MODEL));
        assert_eq!(config.derived_models.len(), 1);
        let route = &config.derived_models[0];
        assert_eq!(route.routing_id, "kimi-k2.7");
        assert_eq!(route.upstream, "cliproxy");
        assert_eq!(route.upstream_model, "openai-compat--kimi-k2.7");
        assert_eq!(route.display_name, "Kimi K2.7");
        // Derived routes participate in routing alongside the defaults.
        let decision = crate::routing::decide(&config, br#"{"model":"kimi-k2.7"}"#);
        assert_eq!(decision.branch, crate::routing::Branch::Gpt);
        assert_eq!(
            decision.route.unwrap().upstream_model,
            "openai-compat--kimi-k2.7"
        );
        let default_still_routes = crate::routing::decide(&config, br#"{"model":"gpt-5.6-sol"}"#);
        assert_eq!(default_still_routes.branch, crate::routing::Branch::Gpt);
    }

    #[test]
    fn provider_routing_id_colliding_with_default_route_is_rejected() {
        let error = prepare_error(&provider_toml(
            r#"
                [[openai-providers.models]]
                name = "some/upstream-model"
                routing-id = "gpt-5.6-sol"
                display-name = "Impostor"
            "#,
        ));
        assert!(
            error.contains("duplicate model routing-id: gpt-5.6-sol"),
            "{error}"
        );
    }

    #[test]
    fn providers_are_rejected_in_external_mode_only() {
        let toml = format!(
            r#"
                [upstreams.cliproxy]
                mode = "external"
                base-url = "http://127.0.0.1:8317"
                {}
            "#,
            provider_toml(KIMI_MODEL)
        );
        let error = prepare_error(&toml);
        assert!(error.contains("mode = \"managed\""), "{error}");
        // Stub mode is the test backend; providers are allowed so routing can
        // be exercised without a live child.
        let toml = format!(
            r#"
                [upstreams.cliproxy]
                mode = "stub"
                {}
            "#,
            provider_toml(KIMI_MODEL)
        );
        parse_and_prepare(&toml);
    }

    #[test]
    fn provider_base_url_must_be_https() {
        for base in ["http://api.fireworks.ai/v1", "ftp://x.example", "not a url"] {
            let toml =
                provider_toml(KIMI_MODEL).replace("https://api.fireworks.ai/inference/v1", base);
            let error = prepare_error(&toml);
            assert!(error.contains("base-url"), "{base}: {error}");
        }
    }

    #[test]
    fn provider_field_validation_rejects_bad_values() {
        let toml = provider_toml(KIMI_MODEL).replace("fireworks", "fire works!");
        let error = prepare_error(&toml);
        assert!(error.contains("may only contain"), "{error}");
        // api-key belongs in secrets.toml, never in the config file.
        let toml = provider_toml(KIMI_MODEL).replace(
            "[[openai-providers]]",
            "[[openai-providers]]\napi-key = \"nope\"",
        );
        assert!(
            toml::from_str::<Config>(&toml).is_err(),
            "inline api-key must be rejected"
        );
        let error = prepare_error(&provider_toml("models = []"));
        assert!(error.contains("defines no models"), "{error}");
    }

    #[test]
    fn duplicate_provider_names_and_routing_ids_are_rejected() {
        // Same provider name, distinct routing-ids: the provider-name check fires.
        let second = provider_toml(KIMI_MODEL).replace("kimi-k2.7", "kimi-other");
        let error = prepare_error(&format!("{}\n{}", provider_toml(KIMI_MODEL), second));
        assert!(error.contains("duplicate openai-provider name"), "{error}");
        // Distinct provider names, same routing-id: the routing-id check fires.
        let second = provider_toml(KIMI_MODEL).replace("\"fireworks\"", "\"together\"");
        let error = prepare_error(&format!("{}\n{}", provider_toml(KIMI_MODEL), second));
        assert!(
            error.contains("duplicate model routing-id: kimi-k2.7"),
            "{error}"
        );
    }

    #[test]
    fn prepare_is_idempotent() {
        let mut config: Config = toml::from_str(&provider_toml(KIMI_MODEL)).unwrap();
        config.prepare().unwrap();
        let first = config.derived_models.clone();
        config.prepare().unwrap();
        assert_eq!(config.derived_models, first);
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
