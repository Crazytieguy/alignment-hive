use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::client_window::{UsageScale, client_context_window};

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

/// The Codex backend's effective input limit for the built-in GPT routes:
/// 272K served with a 95% usable multiplier (backend-advertised
/// `effective_context_window_percent`, verified in the codex-rs client).
/// Load-bearing as the `M` in the translated `prompt is too long` overflow
/// error (see [`crate::overflow`]). Setup declares
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS` at this same value: auto-compaction
/// (declared − 20K output reserve) still leads the cap by 20K, and the
/// overflow backstop covers the rest; `doctor` flags a declaration raised
/// past the cap.
pub(crate) const GPT_CONTEXT_WINDOW: u64 = 258_400;

/// The Codex-native upstream model IDs behind the built-in routes, with
/// their display names.
const CODEX_NATIVE_MODELS: [(&str, &str); 4] = [
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
];

/// Whether `upstream_model` is served by the Codex backend. Derived aliases
/// (`openai-compat--*`) and hand-written `[[models]]` entries pointing at
/// other backends do not qualify.
#[must_use]
pub(crate) fn is_codex_native_model(upstream_model: &str) -> bool {
    is_built_in(&CODEX_NATIVE_MODELS, upstream_model)
}

fn is_built_in(table: &[(&str, &str)], upstream_model: &str) -> bool {
    table.iter().any(|(model, _)| *model == upstream_model)
}

/// Which backend context-overflow dialect this route speaks, if the router
/// has verified one (see [`crate::overflow`]). Checked against the built-in
/// model lists rather than the family alone: a hand-written `[[models]]`
/// entry inherits `family = "gpt"` by default but may point anywhere.
#[must_use]
pub(crate) fn overflow_dialect(route: &ModelRoute) -> Option<crate::overflow::OverflowDialect> {
    use crate::overflow::OverflowDialect;
    match route.family {
        ModelFamily::Gpt => {
            is_codex_native_model(&route.upstream_model).then_some(OverflowDialect::Codex)
        }
        ModelFamily::Grok => {
            is_built_in(&GROK_MODELS, &route.upstream_model).then_some(OverflowDialect::Xai)
        }
        // Any host can sit behind an openai-compat route; their error
        // strings are unknown.
        ModelFamily::OpenAiCompat => None,
    }
}

/// The xAI-native upstream model IDs behind the built-in Grok routes, with
/// their display names. Only the flagship and its still-served predecessor
/// ship — 4.5 stays so existing per-user agents keep resolving: the same
/// OAuth also exposes `grok-4.3`, the `grok-4.20-*` snapshots and the
/// `grok-3-mini*` pair, none of which are characterised well enough to
/// recommend.
const GROK_MODELS: [(&str, &str); 2] = [("grok-4.6", "Grok 4.6"), ("grok-4.5", "Grok 4.5")];

/// The context window `CLIProxyAPI`'s embedded model registry declares for
/// the built-in Grok routes.
const GROK_CONTEXT_WINDOW: u64 = 500_000;

/// Time bounds on the xAI search side call.
///
/// Deliberately no concurrency cap: how many searches may run at once is the
/// user's own xAI subscription's business, and a parallel sweep should meet
/// that limit rather than an invented local one.
///
/// Public only so tests can shrink these; production always uses [`Default`].
#[derive(Clone, Copy, Debug)]
pub struct XaiSearchLimits {
    /// How long the request handler WAITS for sources. Never cancels the
    /// upstream read — see `proxy::XaiSearchTasks`.
    pub harvest_timeout: std::time::Duration,
    /// Upper bound on reading a stream to its end, so a wedged upstream
    /// cannot pin a worker for the process's lifetime.
    pub drain_timeout: std::time::Duration,
}

impl Default for XaiSearchLimits {
    fn default() -> Self {
        Self {
            // Sources are measured at 3–5s; this only bounds a wedged
            // upstream, and is still far under the 20–70s legacy LLM path.
            harvest_timeout: std::time::Duration::from_secs(45),
            drain_timeout: std::time::Duration::from_mins(3),
        }
    }
}

/// Which vendor family serves a route: what the model *is*, which the effort
/// suffix, the overflow dialect, the search backend, and the family label in
/// logs and captures care about. Where a request is *sent* (Anthropic or the
/// `CLIProxyAPI` child) is a separate question, answered by whether a route
/// matched at all.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelFamily {
    /// Codex-native slugs.
    #[default]
    Gpt,
    /// xAI models reached through the child's xAI OAuth.
    Grok,
    /// Routes derived from `[[openai-providers]]`.
    OpenAiCompat,
}

impl ModelFamily {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gpt => "gpt",
            Self::Grok => "grok",
            Self::OpenAiCompat => "openai-compat",
        }
    }
}

// No `Debug` outside tests: `ingress_token` and the providers' keys must not
// reach a log through `{:?}`.
#[serde_inline_default]
#[derive(Clone, Deserialize)]
#[cfg_attr(test, derive(Debug))]
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
    #[serde_inline_default(100 * 1024 * 1024)]
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

    /// Optional Grok (xAI) family. Disabled by default: an install that
    /// never opts in keeps exactly today's routes and doctor checks.
    #[serde(default)]
    pub grok: GrokConfig,

    /// Optional request/response capture settings.
    #[serde(default)]
    pub capture: CaptureConfig,

    /// `WebSearch` sub-call handling; see [`crate::websearch`].
    #[serde(default)]
    pub web_search: WebSearchConfig,

    /// What Claude Code believes routed models' context windows are.
    /// [`Config::load`] reads it from Claude Code's own settings, so it is
    /// normally absent here; an explicit value is the escape hatch for a
    /// settings file the router cannot see (a project-level override, when
    /// the service was started from elsewhere).
    pub declared_context_window: Option<u64>,

    /// Routes the router generates rather than reads: the built-in Grok
    /// family when `[grok] enabled`, plus one per `[[openai-providers]]`
    /// model entry. Rebuilt by [`Config::prepare`], never read from TOML.
    ///
    /// Deliberately not merged into `models`: that field carries a serde
    /// default, which a single user-written `[[models]]` block replaces
    /// wholesale — taking every generated route with it.
    #[serde(skip)]
    pub generated_models: Vec<ModelRoute>,

    #[serde(skip)]
    pub xai_search: XaiSearchLimits,
}

/// The optional Grok family. `enabled` gates the built-in routes, the
/// doctor auth/model checks, and `login grok`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GrokConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Rescale the built-in Grok routes' reported usage so auto-compaction
    /// fires at their real windows rather than at the single declared one.
    /// See [`ModelRoute::context_window_scaling`]; off by default, because
    /// clipping keeps every displayed token count true.
    #[serde(default)]
    pub context_window_scaling: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WebSearchConfig {
    #[serde(default)]
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

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
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
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SecretsFile {
    #[serde(default)]
    openai_providers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProviderModel {
    /// Exact model ID on the provider (e.g. `accounts/fireworks/models/...`).
    pub name: String,
    /// The model ID Claude Code requests (becomes a routed model).
    pub routing_id: String,
    pub display_name: String,

    /// The model's real context window in tokens. Discovered from the host at
    /// startup ([`crate::discovery`]); set it only to override a host whose
    /// catalog reports nothing useful.
    pub context_window: Option<u64>,

    /// Rescale this route's reported usage so Claude Code compacts at the
    /// model's real window rather than at the single one it believes every
    /// routed model has. Only a window *larger* than the declared one can be
    /// scaled — see [`crate::client_window::UsageScale`].
    #[serde(default)]
    pub context_window_scaling: bool,

    /// `OpenRouter` only: route this model solely to sub-providers whose
    /// window is at least this many tokens. The service picks them from the
    /// host's endpoint list at start ([`crate::discovery`]) and pins them
    /// per request through the child config; a model that asks for this and
    /// has no applicable selection is not served at all rather than served
    /// unpinned. Independent of `context-window`, which stays the route's
    /// window claim.
    pub min_context_window: Option<u64>,

    /// The sub-provider slugs the service pinned for this model, from the
    /// last successful lookup for at least [`Self::min_context_window`].
    /// Filled by [`crate::discovery::apply_cached_windows`]; never read from
    /// TOML. `None` with `min_context_window` set means: not served.
    #[serde(skip)]
    pub pinned_providers: Option<Vec<String>>,
}

impl ProviderModel {
    /// Whether this model may be served: everything but a model that asked
    /// for sub-provider pinning and has no applicable selection.
    #[must_use]
    pub fn is_served(&self) -> bool {
        self.min_context_window.is_none() || self.pinned_providers.is_some()
    }
}

/// Whether `base_url` is `OpenRouter`, the one host with sub-provider routing
/// and the only one whose requests may carry a `provider` preference. Exact
/// host match on the parsed URL: a lookalike such as
/// `openrouter.ai.example.com` is another host.
#[must_use]
pub fn is_openrouter(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).is_ok_and(|url| url.host_str() == Some("openrouter.ai"))
}

/// The internal `CLIProxyAPI` alias for a provider model. Routing IDs are
/// globally unique and charset-restricted (validation), so prefixing alone is
/// injective; the provider name would add nothing but ambiguity.
#[must_use]
pub fn derived_alias(routing_id: &str) -> String {
    format!("{DERIVED_ALIAS_PREFIX}{routing_id}")
}

#[serde_inline_default]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ModelRoute {
    pub routing_id: String,

    #[serde_inline_default(CLIPROXY_UPSTREAM.to_string())]
    pub upstream: String,

    pub upstream_model: String,
    pub display_name: String,

    /// Which vendor family serves this route. Read from TOML (defaulting to
    /// `gpt`) so a hand-written entry pointing at a Grok upstream gets the
    /// right effort suffix and overflow dialect; every generated route sets
    /// it explicitly.
    #[serde(default)]
    pub family: ModelFamily,

    /// The model's real context window in tokens. See [`ProviderModel`].
    pub context_window: Option<u64>,

    /// See [`ProviderModel::context_window_scaling`].
    #[serde(default)]
    pub context_window_scaling: bool,

    /// Computed by [`Config::prepare`] from the two fields above and the
    /// client-side window for this routing ID; never read from TOML.
    #[serde(skip)]
    pub usage_scale: Option<UsageScale>,

    /// See [`ProviderModel::min_context_window`]; generated routes only.
    #[serde(skip)]
    pub min_context_window: Option<u64>,

    /// See [`ProviderModel::pinned_providers`]; generated routes only.
    #[serde(skip)]
    pub pinned_providers: Option<Vec<String>>,
}

impl ModelRoute {
    /// The scale this route's reported usage needs, if any. `None` when
    /// scaling is off or the real window already matches what the client
    /// believes (an identity scale is a no-op, not an error).
    fn scale_for(&self, declared: Option<u64>) -> Option<UsageScale> {
        let actual = self
            .context_window
            .filter(|_| self.context_window_scaling)?;
        let client = client_context_window(declared);
        if client == actual {
            return None;
        }
        UsageScale::new(client, actual)
    }

    /// The window this route asked its sub-providers for when the service
    /// has no applicable selection, so the route is not served at all
    /// (see [`ProviderModel::min_context_window`]).
    #[must_use]
    pub fn unserved_min_window(&self) -> Option<u64> {
        self.min_context_window
            .filter(|_| self.pinned_providers.is_none())
    }
}

// No `Debug` outside tests: `api_key` must not reach a log through `{:?}`.
#[serde_inline_default]
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct UpstreamConfig {
    #[serde(default)]
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
    #[serde_inline_default(10 * 1024 * 1024)]
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
    BTreeMap::from([(CLIPROXY_UPSTREAM.to_string(), UpstreamConfig::default())])
}

/// Renders the commented `[[models]]` template section from the actual
/// defaults, so the shipped template can never drift from `default_models()`.
fn template_models_section(models: &[ModelRoute]) -> String {
    models
        .iter()
        .map(|route| {
            let context_window = route
                .context_window
                .map(|window| format!("#context-window = {window}\n"))
                .unwrap_or_default();
            format!(
                "#[[models]]\n#routing-id = \"{}\"\n#upstream = \"{}\"\n#upstream-model = \
                 \"{}\"\n#display-name = \"{}\"\n{context_window}",
                route.routing_id, route.upstream, route.upstream_model, route.display_name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders the commented `[grok]` template section, with the built-in route
/// list generated from [`GROK_MODELS`] so the template can never drift from
/// what the code actually serves.
fn template_grok_section() -> String {
    let routes = GROK_MODELS
        .into_iter()
        .map(|(model, _)| format!("#   {model} ({GROK_CONTEXT_WINDOW} tokens)"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# Optional Grok (xAI) family: rides the same managed CLIProxyAPI child\n# under an xAI \
         subscription OAuth login (`model-router login grok`).\n# Off by default. Built-in routes \
         when enabled:\n{routes}\n#[grok]\n# Default: false\n#enabled = true\n# Rescale reported \
         usage so auto-compaction fires at Grok's real window instead of at\n# \
         CLAUDE_CODE_MAX_CONTEXT_TOKENS. Off by default: clipping keeps every\n# displayed token \
         count true.\n# Default: false\n#context-window-scaling = true\n"
    )
}

/// The fully static provider / context-window section of the config
/// template. Kept out of the interpolated body so `template` stays readable.
const TEMPLATE_PROVIDERS_SECTION: &str = r#"# OpenAI-compatible providers (managed mode only): the managed CLIProxyAPI
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
#name = "moonshotai/kimi-k3"
#routing-id = "kimi-k3"
#display-name = "Kimi K3"
# Claude Code sizes context client-side from the model ID, so it believes every
# routed model has the one window declared by CLAUDE_CODE_MAX_CONTEXT_TOKENS.
# Setting this rescales the route's reported usage so auto-compaction fires at
# the model's real window instead. The real window is discovered from the host
# at startup (cached in the state dir). Only larger-than-declared windows can
# be scaled; a smaller one needs a lower CLAUDE_CODE_MAX_CONTEXT_TOKENS.
# Cost: this route's token counts read in the declared coordinate system.
#context-window-scaling = true
# Override for the discovered window, for hosts whose catalog reports none.
#context-window = 1048576
# OpenRouter only: route this model solely to sub-providers serving at least
# this many tokens (OpenRouter otherwise routes to any of them, and their
# windows differ). The service picks and pins them at start; `verify-providers`
# shows which. A model with no qualifying sub-provider is not served.
#min-context-window = 1000000

# What Claude Code believes routed models' context windows are. Read from
# ~/.claude/settings.json (or the project's) at startup, so it normally needs
# no entry here — set it only when a settings file the service cannot see
# holds the real value.
# declared-context-window = {declared_context_window}
"#;

/// The built-in routes of one family: one bare routing ID per model.
fn built_in_routes(
    table: &[(&str, &str)],
    family: ModelFamily,
    context_window: u64,
    scaling: bool,
) -> Vec<ModelRoute> {
    table
        .iter()
        .map(|(model, display_name)| ModelRoute {
            routing_id: model.to_string(),
            upstream: CLIPROXY_UPSTREAM.to_string(),
            upstream_model: model.to_string(),
            display_name: display_name.to_string(),
            family,
            context_window: Some(context_window),
            context_window_scaling: scaling,
            ..ModelRoute::default()
        })
        .collect()
}

fn default_models() -> Vec<ModelRoute> {
    built_in_routes(
        &CODEX_NATIVE_MODELS,
        ModelFamily::Gpt,
        GPT_CONTEXT_WINDOW,
        false,
    )
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
        config.resolve_declared_context_window();
        config.prepare()?;
        Ok(config)
    }

    /// Reads the context window Claude Code was configured with, so it does
    /// not have to be restated here. An explicit `declared-context-window`
    /// wins — it is the escape hatch for a project-level settings override,
    /// which a service started outside the project cannot see.
    fn resolve_declared_context_window(&mut self) {
        if self.declared_context_window.is_some() {
            return;
        }
        let home = crate::state::home_dir();
        let cwd = std::env::current_dir().unwrap_or_default();
        self.declared_context_window = crate::client_window::resolve(home.as_deref(), &cwd).value();
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
        self.rebuild_generated_models();
        self.validate()?;
        self.compute_usage_scales();
        Ok(())
    }

    /// Resolves every route's usage scale.
    fn compute_usage_scales(&mut self) {
        let declared = self.declared_context_window;
        for route in self
            .models
            .iter_mut()
            .chain(self.generated_models.iter_mut())
        {
            route.usage_scale = route.scale_for(declared);
        }
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

    /// Every route the router serves: configured `[[models]]` plus the
    /// generated ones (built-in Grok, `[[openai-providers]]`).
    pub fn effective_models(&self) -> impl Iterator<Item = &ModelRoute> {
        self.models.iter().chain(self.generated_models.iter())
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

    /// Rebuilds every generated route. The Grok family contributes nothing
    /// when disabled, so a config that never opts in is unchanged.
    fn rebuild_generated_models(&mut self) {
        let derived = self.openai_providers.iter().flat_map(|provider| {
            provider.models.iter().map(|model| ModelRoute {
                routing_id: model.routing_id.clone(),
                upstream: CLIPROXY_UPSTREAM.to_string(),
                upstream_model: derived_alias(&model.routing_id),
                display_name: model.display_name.clone(),
                family: ModelFamily::OpenAiCompat,
                context_window: model.context_window,
                context_window_scaling: model.context_window_scaling,
                min_context_window: model.min_context_window,
                pinned_providers: model.pinned_providers.clone(),
                ..ModelRoute::default()
            })
        });
        let grok = if self.grok.enabled {
            built_in_routes(
                &GROK_MODELS,
                ModelFamily::Grok,
                GROK_CONTEXT_WINDOW,
                self.grok.context_window_scaling,
            )
        } else {
            Vec::new()
        };
        self.generated_models = grok.into_iter().chain(derived).collect();
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
        validate_anthropic_base_url(&self.anthropic_upstream_base)?;
        ensure!(
            self.max_request_body_bytes > 0,
            "max-request-body-bytes must be greater than zero"
        );
        if let Some(token) = &self.ingress_token {
            ensure!(!token.is_empty(), "ingress-token cannot be empty");
            ensure!(
                token.bytes().all(|byte| is_slug_byte(byte) || byte == b'~'),
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

        ensure!(
            self.declared_context_window.is_none_or(|window| window > 0),
            "declared-context-window must be greater than zero"
        );

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
                !route
                    .display_name
                    .bytes()
                    .any(|byte| byte.is_ascii_control()),
                "display-name contains control characters for {}",
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
            self.validate_context_window(route)?;
        }

        self.validate_openai_providers(cliproxy)
    }

    /// Context-window fields on one route.
    fn validate_context_window(&self, route: &ModelRoute) -> anyhow::Result<()> {
        if let Some(window) = route.context_window {
            ensure!(
                window > 0,
                "context-window must be greater than zero for {}",
                route.routing_id
            );
        }
        if route.context_window_scaling {
            let client = client_context_window(self.declared_context_window);
            // See `UsageScale`: only scaling down is sound.
            if let Some(actual) = route.context_window {
                ensure!(
                    actual >= client,
                    "model {} sets context-window {actual}, below the {client} Claude Code \
                     believes it has: scaling cannot protect a model whose window is smaller \
                     than the declared one. Lower CLAUDE_CODE_MAX_CONTEXT_TOKENS to {actual} \
                     instead (and scale the larger routes back up from there)",
                    route.routing_id
                );
            }
        }
        Ok(())
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
                provider.name.bytes().all(is_slug_byte),
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
                    !model.routing_id.is_empty(),
                    "openai-provider {} model {} has an empty routing-id",
                    provider.name,
                    model.name
                );
                ensure!(
                    model.routing_id.bytes().all(is_slug_byte),
                    "openai-provider {} model {} routing-id {:?} may only contain \
                     alphanumerics, -, _, . (it becomes a CLIProxyAPI model alias)",
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
                if let Some(min) = model.min_context_window {
                    ensure!(
                        min > 0,
                        "openai-provider {} model {} min-context-window must be greater than zero",
                        provider.name,
                        model.name
                    );
                    ensure!(
                        is_openrouter(&provider.base_url),
                        "openai-provider {} model {} sets min-context-window, which only \
                         OpenRouter supports (it pins the sub-providers OpenRouter may route \
                         to); a host without sub-provider routing serves one window — set \
                         `context-window` instead",
                        provider.name,
                        model.name
                    );
                }
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
# loopback on the configured port.
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

{providers}# Maximum accepted inbound request-body size in bytes. Oversized requests get
# a 413 error instead of being buffered without bound.
# Default: {max_request_body_bytes}
# max-request-body-bytes = {max_request_body_bytes}

# Ingress token: the router only accepts requests under the /t/<token>/ path
# prefix (ANTHROPIC_BASE_URL includes it), so other local processes cannot
# spend your Codex subscription through the loopback port. When unset,
# `serve` uses a create-once random token stored in the state dir. Set it
# only to pin a known value; URL-safe characters only.
# ingress-token = "replace-with-a-random-token"

# GPT routing is exact-match only. Requests for every other model go to
# Anthropic. By default the four GPT routes below are enabled; writing
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

{grok}
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
# Default: {capture_max_response_body_bytes}
#max-response-body-bytes = {capture_max_response_body_bytes}
"#,
            bind_address = defaults.bind_address,
            port = defaults.port,
            max_request_body_bytes = defaults.max_request_body_bytes,
            anthropic_base = defaults.anthropic_upstream_base,
            models = template_models_section(&defaults.models),
            providers = TEMPLATE_PROVIDERS_SECTION
                .replace("{declared_context_window}", &GPT_CONTEXT_WINDOW.to_string()),
            grok = template_grok_section(),
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

/// The charset shared by every identifier that becomes a `CLIProxyAPI` name.
fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn validate_anthropic_base_url(value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(value).context("anthropic-upstream-base is not a valid URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "anthropic-upstream-base must start with http:// or https://"
    );
    ensure!(
        url.host_str().is_some(),
        "anthropic-upstream-base has no host"
    );
    ensure!(
        url.query().is_none(),
        "anthropic-upstream-base cannot carry a query string"
    );
    Ok(())
}

fn validate_cliproxy_base_url(value: &str) -> anyhow::Result<()> {
    const FIELD: &str = "[upstreams.cliproxy] base-url";
    const BOUNDARY: &str = "[upstreams.cliproxy] base-url must use a loopback IP literal \
        (127.0.0.0/8 or ::1); hostnames (including localhost) and non-loopback \
        addresses are rejected because this is the GPT credential injection boundary";

    let url = reqwest::Url::parse(value).with_context(|| format!("invalid {FIELD}; {BOUNDARY}"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "{FIELD} must start with http:// or https://; {BOUNDARY}"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{FIELD} has no host; {BOUNDARY}"))?;
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ip = ip_literal
        .parse::<IpAddr>()
        .map_err(|_| anyhow::anyhow!("{FIELD} host {host:?} is not an IP; {BOUNDARY}"))?;
    ensure!(
        ip.is_loopback(),
        "{FIELD} host {host:?} is not loopback; {BOUNDARY}"
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
            validate_cliproxy_base_url(base_url)?;
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

/// Test fixture: a parsed and prepared config, panicking on any error.
#[cfg(test)]
pub(crate) fn parse_and_prepare(source: &str) -> Config {
    let mut config: Config = toml::from_str(source).unwrap();
    config.prepare().unwrap();
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_error(source: &str) -> String {
        let mut config: Config = toml::from_str(source).unwrap();
        format!("{:#}", config.prepare().unwrap_err())
    }

    #[test]
    fn absent_upstreams_table_defaults_to_managed_cliproxy() {
        let config = parse_and_prepare("");
        assert_eq!(config.upstreams.keys().collect::<Vec<_>>(), ["cliproxy"]);
        assert_eq!(config.upstreams["cliproxy"], UpstreamConfig::default());
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
    fn template_parses_to_the_defaults_when_left_commented() {
        let config = parse_and_prepare(&Config::template());
        let defaults = Config::default();
        assert_eq!(config.port, defaults.port);
        assert_eq!(config.upstreams, defaults.upstreams);
        assert_eq!(config.models, defaults.models);
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
            parse_and_prepare(&format!(
                r#"
                    [upstreams.cliproxy]
                    mode = "external"
                    base-url = {base:?}
                    api-key = "local-gateway-secret"
                "#
            ));
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
            assert!(error.contains("[upstreams.cliproxy] base-url"), "{error}");
            assert!(error.contains("loopback IP literal"), "{error}");
            assert!(error.contains("credential injection boundary"), "{error}");
        }
    }

    #[test]
    fn anthropic_upstream_base_must_be_a_plain_http_url() {
        for base in [
            "api.anthropic.com",
            "ftp://x.example",
            "https://",
            "https://x.example/?q=1",
        ] {
            let config: Config =
                toml::from_str(&format!("anthropic-upstream-base = {base:?}")).unwrap();
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains("anthropic-upstream-base"), "{base}: {error}");
        }
        parse_and_prepare("anthropic-upstream-base = \"https://proxy.example/v1\"");
    }

    #[test]
    fn removed_flat_fields_and_unknown_nested_fields_are_rejected() {
        for source in [
            "gpt-upstream-base = \"stub\"",
            "gpt-upstream-api-key = \"secret\"",
            "[upstreams.cliproxy]\nmode = \"stub\"\nextra = true",
            "[[models]]\nrouting-id = \"route\"\nupstream-model = \"model\"\ndisplay-name = \"Model\"\nextra = true",
            "[grok]\nenabled = true\nnope = 1",
            "[[models]]\nrouting-id = \"a\"\nupstream-model = \"m\"\ndisplay-name = \"A\"\nfamily = \"gemini\"",
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
        assert_eq!(config.generated_models.len(), 1);
        let route = &config.generated_models[0];
        assert_eq!(route.routing_id, "kimi-k2.7");
        assert_eq!(route.upstream, "cliproxy");
        assert_eq!(route.upstream_model, "openai-compat--kimi-k2.7");
        assert_eq!(route.display_name, "Kimi K2.7");
        // Derived routes participate in routing alongside the defaults.
        let decision = crate::routing::decide(&config, br#"{"model":"kimi-k2.7"}"#);
        assert_eq!(
            decision.route.unwrap().upstream_model,
            "openai-compat--kimi-k2.7"
        );
        let default_still_routes = crate::routing::decide(&config, br#"{"model":"gpt-5.6-sol"}"#);
        assert!(default_still_routes.route.is_some());
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
    fn min_context_window_is_positive_and_openrouter_only() {
        let error = prepare_error(&provider_toml(KIMI_MODEL).replace(
            "display-name = \"Kimi K2.7\"",
            "display-name = \"Kimi K2.7\"\nmin-context-window = 1000000",
        ));
        assert!(error.contains("only OpenRouter supports"), "{error}");
        let error = prepare_error(&kimi_toml("min-context-window = 0", false));
        assert!(error.contains("greater than zero"), "{error}");
    }

    #[test]
    fn is_openrouter_matches_the_exact_host_only() {
        assert!(is_openrouter("https://openrouter.ai/api/v1"));
        assert!(!is_openrouter("https://openrouter.ai.example.com/api/v1"));
        assert!(!is_openrouter("https://api.fireworks.ai/inference/v1"));
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
        // A provider colliding with a built-in route.
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
    fn prepare_is_idempotent_across_legacy_normalization() {
        let mut config: Config = toml::from_str(
            "[upstreams.codex]\nmode = \"stub\"\n\n[[models]]\nrouting-id = \"r\"\nupstream = \"codex\"\nupstream-model = \"m\"\ndisplay-name = \"M\"\n",
        )
        .unwrap();
        config.prepare().unwrap();
        config.prepare().unwrap();
        assert_eq!(config.upstreams.keys().collect::<Vec<_>>(), ["cliproxy"]);
        assert_eq!(config.models[0].upstream, "cliproxy");
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

    // ---- context windows ----

    /// One `OpenRouter` model with the context fields under test, under the
    /// declaration setup writes.
    fn kimi_toml(window: &str, scaling: bool) -> String {
        format!(
            r#"
                declared-context-window = {GPT_CONTEXT_WINDOW}
                [[openai-providers]]
                name = "openrouter"
                base-url = "https://openrouter.ai/api/v1"
                [[openai-providers.models]]
                name = "moonshotai/kimi-k3"
                routing-id = "kimi-k3"
                display-name = "Kimi K3"
                {window}
                context-window-scaling = {scaling}
            "#
        )
    }

    fn kimi(window: u64, scaling: bool) -> String {
        kimi_toml(&format!("context-window = {window}"), scaling)
    }

    fn route<'a>(config: &'a Config, routing_id: &str) -> &'a ModelRoute {
        config
            .effective_models()
            .find(|route| route.routing_id == routing_id)
            .unwrap()
    }

    #[test]
    fn derived_route_inherits_window_and_scale() {
        let config = parse_and_prepare(&kimi(1_000_000, true));
        let route = route(&config, "kimi-k3");
        assert_eq!(route.context_window, Some(1_000_000));
        assert!(route.context_window_scaling);
        // A real 1M-token conversation reports as the declared window, so
        // Claude Code compacts at the model's real limit.
        assert_eq!(
            route.usage_scale.unwrap().apply(1_000_000),
            GPT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn scaling_is_none_when_the_windows_already_agree() {
        let config = parse_and_prepare(&kimi(GPT_CONTEXT_WINDOW, true));
        assert!(route(&config, "kimi-k3").usage_scale.is_none());
    }

    #[test]
    fn scaling_a_window_below_the_declared_one_is_rejected() {
        let error = prepare_error(&kimi(125_000, true));
        assert!(
            error.contains(&format!("below the {GPT_CONTEXT_WINDOW}")),
            "{error}"
        );
        assert!(
            error.contains("Lower CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            "{error}"
        );
    }

    #[test]
    fn overflow_translation_is_armed_for_built_in_models_only() {
        use crate::overflow::OverflowDialect;
        let config =
            parse_and_prepare(&format!("{}[grok]\nenabled = true\n", kimi_toml("", false)));
        let dialect = |routing_id: &str| overflow_dialect(route(&config, routing_id));
        assert_eq!(dialect("gpt-5.6-sol"), Some(OverflowDialect::Codex));
        assert_eq!(dialect("grok-4.5"), Some(OverflowDialect::Xai));
        assert_eq!(dialect("kimi-k3"), None);
        // A hand-written route inherits the family but not the verified backend.
        let hand_written = parse_and_prepare(
            "[[models]]\nrouting-id = \"a\"\nupstream-model = \"m\"\ndisplay-name = \"A\"\n\n\
             [[models]]\nrouting-id = \"b\"\nupstream-model = \"m2\"\ndisplay-name = \"B\"\nfamily = \"grok\"\n",
        );
        assert_eq!(overflow_dialect(&hand_written.models[0]), None);
        assert_eq!(overflow_dialect(&hand_written.models[1]), None);
    }

    #[test]
    fn a_scaling_route_without_a_window_waits_for_discovery() {
        // `serve` fills these in from the host; until then the route is
        // simply unscaled rather than a config error.
        let config = parse_and_prepare(&kimi_toml("", true));
        let route = route(&config, "kimi-k3");
        assert!(route.context_window.is_none());
        assert!(route.usage_scale.is_none());
    }

    #[test]
    fn windows_must_be_positive() {
        assert!(prepare_error(&kimi(0, true)).contains("greater than zero"));
    }

    // ---- Grok family (optional) ----

    #[test]
    fn grok_routes_exist_only_when_enabled_and_survive_a_hand_written_models_block() {
        for source in ["", "[grok]\nenabled = false\n"] {
            let config = parse_and_prepare(source);
            assert!(
                !config
                    .effective_models()
                    .any(|r| r.family == ModelFamily::Grok),
                "{source:?}"
            );
        }
        let config = parse_and_prepare(
            "[[models]]\nrouting-id = \"mine\"\nupstream-model = \"m\"\ndisplay-name = \"M\"\n\n[grok]\nenabled = true\n",
        );
        assert_eq!(config.models.len(), 1);
        for (model, _) in GROK_MODELS {
            let route = route(&config, model);
            assert_eq!(route.family, ModelFamily::Grok);
            assert_eq!(route.upstream_model, model);
        }
    }

    #[test]
    fn grok_scaling_follows_the_flag() {
        let config = parse_and_prepare("[grok]\nenabled = true\n");
        assert!(route(&config, "grok-4.5").usage_scale.is_none());

        let config = parse_and_prepare("[grok]\nenabled = true\ncontext-window-scaling = true\n");
        let route = route(&config, "grok-4.5");
        let scale = route.usage_scale.expect("grok-4.5 unscaled");
        // A full real window reports as the declared one.
        assert_eq!(
            scale.apply(route.context_window.unwrap()),
            GPT_CONTEXT_WINDOW
        );
    }

    // ---- family field ----

    #[test]
    fn family_defaults_to_gpt_and_is_readable_from_toml() {
        let config = parse_and_prepare(
            "[[models]]\nrouting-id = \"a\"\nupstream-model = \"m\"\ndisplay-name = \"A\"\n\n\
             [[models]]\nrouting-id = \"b\"\nupstream-model = \"m2\"\ndisplay-name = \"B\"\nfamily = \"grok\"\n",
        );
        assert_eq!(config.models[0].family, ModelFamily::Gpt);
        assert_eq!(config.models[1].family, ModelFamily::Grok);
    }

    #[test]
    fn generated_routes_carry_their_family() {
        let config = parse_and_prepare(
            "[grok]\nenabled = true\n\n[[openai-providers]]\nname = \"p\"\nbase-url = \"https://example.test/v1\"\n\n\
             [[openai-providers.models]]\nname = \"vendor/model\"\nrouting-id = \"kimi\"\ndisplay-name = \"Kimi\"\n",
        );
        let family = |routing_id: &str| {
            config
                .effective_models()
                .find(|route| route.routing_id == routing_id)
                .unwrap()
                .family
        };
        assert_eq!(family("gpt-5.6-sol"), ModelFamily::Gpt);
        assert_eq!(family("grok-4.5"), ModelFamily::Grok);
        assert_eq!(family("kimi"), ModelFamily::OpenAiCompat);
    }
}
