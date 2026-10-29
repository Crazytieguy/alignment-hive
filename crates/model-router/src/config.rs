//! The TOML config and its sibling `secrets.toml`: schema, loading,
//! validation, and the commented template. Every construction path must end
//! in [`Config::prepare`], which rebuilds the generated routes (built-in Grok
//! and Muse, one per `[[openai-providers]]` model) and enforces the safety
//! invariants, such as loopback-only binding, a loopback external upstream
//! (the GPT credential boundary) and Muse's Standard mode. The built-in
//! routes live in [`catalog`].

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::client_window::{EnvSetting, UsageScale, client_context_window};

mod catalog;

#[cfg(test)]
pub(crate) use catalog::GPT_CONTEXT_WINDOW;
pub(crate) use catalog::{
    CODEX_DEFAULT_CONTEXT_WINDOW, MUSE_WONDER_FULL, SHIPPED_GPT_ROUTES, codex_default_window,
    forces_text_on_compaction, is_built_in_route, is_codex_native_model, is_muse_contributor_model,
    is_retired_gpt_route, names_auto_model, names_muse_model, names_muse_wonder_full,
    overflow_dialect, shipped_picker_rows, superseded_codex_windows,
};
use catalog::{
    GROK_CONTEXT_WINDOW, GROK_MODELS, MUSE_CONTEXT_WINDOW, MUSE_CONTRIBUTOR_SUFFIX, MUSE_MODELS,
    built_in_routes, default_models,
};

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

/// A model ID split at `CLIProxyAPI`'s `model(effort)` grammar: the base
/// model ID, and the effort a hand-written route pinned there, if any.
/// Model-list lookups compare the base; on Grok routes a pinned effort
/// replaces the per-request one (`routing::effort_qualified_model`).
#[must_use]
pub fn split_effort_suffix(model: &str) -> (&str, Option<&str>) {
    model
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once('('))
        .map_or((model, None), |(base, effort)| (base, Some(effort)))
}

/// Time bounds on the vendor-native (xAI, Meta) search side call.
///
/// Deliberately no concurrency cap: how many searches may run at once is the
/// user's own subscription's business, and a parallel sweep should meet that
/// limit rather than an invented local one.
///
/// Public only so tests can shrink these; production always uses [`Default`].
#[derive(Clone, Copy, Debug)]
pub struct NativeSearchLimits {
    /// How long the request handler WAITS for sources. Never cancels the
    /// upstream read — see `proxy::SearchStreams`.
    pub harvest_timeout: std::time::Duration,
    /// Upper bound on reading a stream to its end, so a wedged upstream
    /// cannot pin a worker for the process's lifetime.
    pub drain_timeout: std::time::Duration,
}

impl Default for NativeSearchLimits {
    fn default() -> Self {
        Self {
            // Sources are measured at 3–5s on xAI and ~2s on Meta; this only
            // bounds a wedged upstream, and is still far under the 20–70s
            // legacy LLM path.
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
    /// Meta models reached through the child's Meta (Muse Code) OAuth.
    Muse,
    /// Routes derived from `[[openai-providers]]`.
    OpenAiCompat,
}

impl ModelFamily {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gpt => "gpt",
            Self::Grok => "grok",
            Self::Muse => "muse",
            Self::OpenAiCompat => "openai-compat",
        }
    }

    /// Whether requested effort must ride the model ID as `CLIProxyAPI`'s
    /// `model(effort)` suffix (see [`crate::routing::effort_qualified_model`]).
    /// Only the xAI path drops `output_config.effort`; the Meta path maps it
    /// to `reasoning.effort` one to one, `max` included (measured against
    /// `CLIProxyAPI` 8.0.30).
    ///
    /// Matched exhaustively on purpose: a new family must decide.
    #[must_use]
    pub const fn effort_via_model_suffix(self) -> bool {
        match self {
            Self::Grok => true,
            Self::Gpt | Self::Muse | Self::OpenAiCompat => false,
        }
    }

    /// The maker the identity block names ([`crate::identity::identity_text`]),
    /// only where the models misattribute it without one: Muse Wonder said it
    /// was "created by Muse" in both live identity runs, while GPT and Grok
    /// name their makers unprompted.
    #[must_use]
    pub const fn maker(self) -> Option<&'static str> {
        match self {
            Self::Muse => Some("Meta"),
            Self::Gpt | Self::Grok | Self::OpenAiCompat => None,
        }
    }

    /// Whether the backend rejects every `tool_choice` but `auto`. Meta
    /// answers `none`, `required` and named choices with a 400 ("only
    /// `\"auto\"` is supported for `tool_choice`", measured 2026-10-15).
    ///
    /// Matched exhaustively on purpose: a new family must decide.
    #[must_use]
    pub const fn tool_choice_auto_only(self) -> bool {
        match self {
            Self::Muse => true,
            Self::Gpt | Self::Grok | Self::OpenAiCompat => false,
        }
    }

    /// Whether the backend may reject a replayed reasoning item that no
    /// assistant message or function call follows before the next message,
    /// so thinking left unanswered in the history is dropped
    /// ([`crate::reasoning_order`]). Meta documents it as a 400 (not enforced
    /// live on 2026-10-19); the child replays no foreign reasoning to the
    /// others.
    ///
    /// Matched exhaustively on purpose: a new family must decide.
    #[must_use]
    pub const fn rejects_unanswered_reasoning(self) -> bool {
        match self {
            Self::Muse => true,
            Self::Gpt | Self::Grok | Self::OpenAiCompat => false,
        }
    }

    /// Whether a cache mark inside a user message becomes a message boundary
    /// ([`crate::cache_marks`]), where the backend caches only up to message
    /// ends and the request's end. Measured on the Codex backend; the
    /// others' caching is unmeasured.
    ///
    /// Matched exhaustively on purpose: a new family must decide.
    #[must_use]
    pub const fn splits_messages_at_cache_marks(self) -> bool {
        match self {
            Self::Gpt => true,
            Self::Grok | Self::Muse | Self::OpenAiCompat => false,
        }
    }

    /// Whether the tools Claude Code adds or withdraws mid-conversation, in
    /// a system message's `tool_addition`/`tool_removal` blocks, must be
    /// declared in `tools[]` ([`crate::tool_changes`]): every translator of
    /// the child keeps only such a message's text.
    ///
    /// Matched exhaustively on purpose: a new family must decide.
    #[must_use]
    pub const fn hoists_tool_changes(self) -> bool {
        match self {
            Self::Gpt | Self::Grok | Self::Muse | Self::OpenAiCompat => true,
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

    /// How the built-in GPT routes authenticate: the Codex subscription
    /// login (default) or an `OpenAI` API key.
    #[serde(default)]
    pub gpt: GptConfig,

    /// Optional Muse (Meta) family. Disabled by default, like `[grok]`.
    #[serde(default)]
    pub muse: MuseConfig,

    /// Optional request/response capture settings.
    #[serde(default)]
    pub capture: CaptureConfig,

    /// `WebSearch` sub-call handling; see [`crate::websearch`].
    #[serde(default)]
    pub web_search: WebSearchConfig,

    /// The HTTPS-proxy front's CA and credential (the CONNECT handling in
    /// [`crate::proxy`]), loaded by `serve`. `None` while the CA cannot be
    /// loaded: Claude Code's own tunnels are refused then, the rest still
    /// pass.
    #[serde(skip)]
    pub interceptor: Option<std::sync::Arc<crate::mitm::Interceptor>>,

    /// Deprecated, with `context-window-scaling`: the window Claude Code
    /// believes routed models have, which scaled routes report usage
    /// against. Normally absent, so the service reads Claude Code's own
    /// setting at startup ([`Config::adopt_client_window`]); an explicit
    /// value was the escape hatch for a settings file the service cannot see.
    pub declared_context_window: Option<u64>,
    /// Set when [`Self::declared_context_window`] came from the client
    /// rather than the config file, so doctor doesn't flag it as deprecated.
    #[serde(skip)]
    pub declared_adopted: bool,

    /// Routes the router generates rather than reads: the built-in Grok and
    /// Muse families when enabled, plus one per `[[openai-providers]]`
    /// model entry. Rebuilt by [`Config::prepare`], never read from TOML.
    ///
    /// Deliberately not merged into `models`: that field carries a serde
    /// default, which a single user-written `[[models]]` block replaces
    /// wholesale — taking every generated route with it.
    #[serde(skip)]
    pub generated_models: Vec<ModelRoute>,

    #[serde(skip)]
    pub native_search: NativeSearchLimits,

    /// The managed child's auth dir ([`Self::managed_auth_dir`]), whose
    /// logins the proxy inspects before forwarding a request that relies on
    /// them: the Muse gate ([`crate::muse::refusal`]) and the exclusion
    /// check ([`crate::exclusions`]). Set by `serve`; `None` refuses every
    /// Muse request and checks no exclusion (an external child's logins are
    /// its own).
    #[serde(skip)]
    pub auth_dir: Option<PathBuf>,
}

/// The optional Grok family. `enabled` gates the built-in routes, the
/// doctor auth/model checks, and `login grok`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GrokConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Deprecated: rescale the built-in Grok routes' reported usage. See
    /// [`ModelRoute::context_window_scaling`].
    #[serde(default)]
    pub context_window_scaling: bool,
}

/// `[gpt]`: the credential behind the built-in GPT routes.
// No `Debug` outside tests: `api_key` must not reach a log through `{:?}`.
#[derive(Clone, Default, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GptConfig {
    #[serde(default)]
    pub auth: GptAuth,

    /// Loaded from `[gpt] api-key` in the sibling `secrets.toml`, never from
    /// the config file, and only under `auth = "api-key"`. `None` there means
    /// the key is missing: the GPT routes have no credential, and doctor says
    /// so.
    #[serde(skip)]
    pub api_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GptAuth {
    /// The Codex OAuth login (`model-router login`), billed to the `ChatGPT`
    /// subscription.
    #[default]
    Subscription,
    /// An `OpenAI` API key, billed per token, served through the managed
    /// child's Codex executor (the Responses API).
    ApiKey,
}

/// The optional Muse family. `enabled` gates the built-in routes, the
/// doctor auth check, and `login muse`; `contributor` opts in to Muse
/// Code's Contributor mode ([`MuseMode`]).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MuseConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Contributor mode, which Meta trains on. Off by default, and inert
    /// while `enabled` is off.
    #[serde(default)]
    pub contributor: bool,
}

impl MuseConfig {
    #[must_use]
    pub const fn mode(&self) -> MuseMode {
        match (self.enabled, self.contributor) {
            (false, _) => MuseMode::Off,
            (true, false) => MuseMode::Standard,
            (true, true) => MuseMode::Contributor,
        }
    }
}

/// How the router talks to Meta, mirroring Muse Code 1.4.9 (measured against
/// a loopback mock): Standard mode sends the plain model ID and no
/// [`crate::muse::CONTRIBUTOR_HEADER`]; Contributor mode the `-contributor`
/// ID and that header set to `1`. [`crate::muse`] says how each is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MuseMode {
    /// `[muse]` is off: the child serves no Meta model at all.
    #[default]
    Off,
    Standard,
    Contributor,
}

impl MuseMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Standard => "Standard",
            Self::Contributor => "Contributor",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WebSearchConfig {
    #[serde(default)]
    pub mode: WebSearchMode,
}

/// How the router answers Claude Code's `WebSearch` sub-call for GPT and
/// open-weights agents. A Grok or Muse agent's search always goes to its own
/// vendor's search, whatever the mode.
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
    /// Pass GPT, open-weights and Claude `WebSearch` sub-calls through
    /// untouched.
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
/// freely-editable config. `[openai-providers]` maps provider name to key;
/// `[gpt] api-key` is the `OpenAI` key for `[gpt] auth = "api-key"`.
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SecretsFile {
    #[serde(default)]
    openai_providers: BTreeMap<String, String>,
    #[serde(default)]
    gpt: GptSecrets,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GptSecrets {
    api_key: Option<String>,
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

    /// Deprecated; see [`ModelRoute::context_window_scaling`].
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
    /// `gpt`) so a hand-written entry pointing at a Grok or Muse upstream
    /// gets the right effort suffix, overflow dialect and search backend;
    /// every generated route sets it explicitly.
    #[serde(default)]
    pub family: ModelFamily,

    /// The model's real context window in tokens. See [`ProviderModel`].
    pub context_window: Option<u64>,

    /// Deprecated in favour of a per-model `autoCompactWindow`, and kept so
    /// existing installs behave as they did: rescale this route's reported
    /// usage so Claude Code compacts at the model's real window rather than
    /// at the single one it believes every routed model has. Only a window
    /// *larger* than the declared one is scaled — see [`UsageScale`].
    #[serde(default)]
    pub context_window_scaling: bool,

    /// Computed by [`Config::prepare`] from the two fields above and the
    /// client-side window; never read from TOML.
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
    /// [`split_effort_suffix`] of this route's `upstream_model`.
    #[must_use]
    pub fn split_upstream_model(&self) -> (&str, Option<&str>) {
        split_effort_suffix(&self.upstream_model)
    }

    /// The scale this route's reported usage needs, if any: `None` when
    /// scaling is off, the real window is unknown, or it is not larger than
    /// the client's (an identity scale is a no-op, and scaling up is unsound).
    fn scale_for(&self, declared: Option<u64>) -> Option<UsageScale> {
        let actual = self
            .context_window
            .filter(|_| self.context_window_scaling)?;
        let client = client_context_window(declared);
        if actual <= client {
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
    let routes = template_route_lines(&GROK_MODELS, GROK_CONTEXT_WINDOW);
    format!(
        "# Optional Grok (xAI) family: rides the same managed CLIProxyAPI child\n# under an xAI \
         subscription OAuth login (`model-router login grok`).\n# Off by default. Built-in routes \
         when enabled:\n{routes}\n#[grok]\n# Default: false\n#enabled = true\n"
    )
}

/// One commented line per built-in route of a family, for its template
/// section.
fn template_route_lines(table: &[(&str, &str)], window: u64) -> String {
    table
        .iter()
        .map(|(model, _)| format!("#   {model} ({window} tokens)"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders the commented `[muse]` template section from [`MUSE_MODELS`], as
/// [`template_grok_section`] does for Grok.
fn template_muse_section() -> String {
    let routes = template_route_lines(&MUSE_MODELS, MUSE_CONTEXT_WINDOW);
    format!(
        "# Optional Muse (Meta) family: rides the same managed CLIProxyAPI child\n# under a Muse \
         Code subscription OAuth login (`model-router login muse`).\n# Off by default. Muse \
         Wonder Full needs the Power or Maximum plan.\n# Built-in routes when enabled:\n\
         {routes}\n#[muse]\n# Default: false\n#enabled = true\n# contributor = true opts in \
         to Contributor mode, which Meta trains on in\n# exchange for more usage: Muse routes \
         then use the -contributor model IDs\n# and carry Muse Code's Contributor header. \
         Default: false\n#contributor = true\n"
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
# The model's real context window is discovered from the host at startup
# (cached in the state dir); `doctor` checks it against where Claude Code
# compacts the route. Override it for hosts whose catalog reports none.
#context-window = 1048576
# OpenRouter only: route this model solely to sub-providers serving at least
# this many tokens (OpenRouter otherwise routes to any of them, and their
# windows differ). The service picks and pins them at start; `verify-providers`
# shows which. A model with no qualifying sub-provider is not served.
#min-context-window = 1000000

"#;

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

    /// Attaches API keys from `secrets.toml` to matching providers, and the
    /// GPT key under `[gpt] auth = "api-key"`. A missing file or missing entry
    /// is not an error (the route runs degraded and doctor/verify-providers
    /// point at it); an unreadable or invalid file is.
    ///
    /// # Errors
    /// Returns an error when the secrets file exists but cannot be read or
    /// parsed.
    pub fn load_secrets(&mut self, path: &Path) -> anyhow::Result<()> {
        let gpt_key = self.gpt.auth == GptAuth::ApiKey;
        if (self.openai_providers.is_empty() && !gpt_key) || !path.exists() {
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
        if gpt_key {
            self.gpt.api_key = secrets.gpt.api_key;
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

    /// Whether the config file itself sets the deprecated
    /// `declared-context-window` (rather than it being adopted).
    pub(crate) const fn declares_window_in_config(&self) -> bool {
        self.declared_context_window.is_some() && !self.declared_adopted
    }

    /// Takes the context window Claude Code was configured with (`client`,
    /// from its environment or settings) as `declared-context-window` unless
    /// the config states one, and rescales the routes for it. An explicit
    /// value wins: it is the escape hatch for a project-level settings
    /// override, which a service started outside the project cannot see.
    pub(crate) fn adopt_client_window(&mut self, client: EnvSetting) {
        if self.declared_context_window.is_none() {
            self.declared_context_window = client.value();
            self.declared_adopted = self.declared_context_window.is_some();
        }
        self.compute_usage_scales();
    }

    /// [`Self::adopt_client_window`] with the value in force for a client
    /// started in `project` (the process environment, else the settings
    /// files).
    pub fn read_client_window(&mut self, home: Option<&Path>, project: &Path) {
        let settings = crate::claude_settings::settings_by_precedence(home, project);
        self.adopt_client_window(crate::client_window::resolve(
            &crate::client_window::WindowEnv::read(),
            &settings,
        ));
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

    /// Logs every route that asks for `context-window-scaling` and runs
    /// unscaled. Never an error: a declaration raised to a route's window or
    /// past it (setup's 1M) with the key left behind must still serve.
    pub fn warn_unscaled_routes(&self) {
        let client = client_context_window(self.declared_context_window);
        for route in self
            .effective_models()
            .filter(|route| route.context_window_scaling && route.usage_scale.is_none())
        {
            if let Some(window) = route.context_window {
                tracing::warn!(
                    route = route.routing_id,
                    window,
                    client,
                    "context-window-scaling is set but the real window is not larger than the \
                     one Claude Code believes; leaving this route unscaled"
                );
            } else {
                tracing::warn!(
                    route = route.routing_id,
                    "context-window-scaling is set but no context window is known; this route \
                     will not be scaled (set `context-window` explicitly to scale it anyway)"
                );
            }
        }
    }

    /// The managed child's auth dir, or `None` when the `cliproxy` upstream
    /// is not managed: the one place that decides whose Meta logins may
    /// vouch for Muse requests ([`crate::muse::refusal`]), for `serve` and
    /// `doctor` alike.
    #[must_use]
    pub fn managed_auth_dir(&self, dirs: &crate::state::Dirs) -> Option<PathBuf> {
        self.is_managed().then(|| dirs.auth_dir())
    }

    /// Whether the `cliproxy` upstream is a child the router manages.
    #[must_use]
    pub fn is_managed(&self) -> bool {
        self.upstreams
            .get(CLIPROXY_UPSTREAM)
            .is_some_and(|upstream| upstream.mode == UpstreamMode::Managed)
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
    /// generated ones (built-in Grok and Muse, `[[openai-providers]]`).
    pub fn effective_models(&self) -> impl Iterator<Item = &ModelRoute> {
        self.models.iter().chain(self.generated_models.iter())
    }

    /// The route clients address as `routing_id`, if any.
    #[must_use]
    pub fn route(&self, routing_id: &str) -> Option<&ModelRoute> {
        self.effective_models()
            .find(|route| route.routing_id == routing_id)
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

    /// Rebuilds every generated route. The Grok and Muse families contribute
    /// nothing when disabled, so a config that never opts in is unchanged.
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
        let muse_routes =
            || built_in_routes(&MUSE_MODELS, ModelFamily::Muse, MUSE_CONTEXT_WINDOW, false);
        let muse = match self.muse.mode() {
            MuseMode::Off => Vec::new(),
            MuseMode::Standard => muse_routes(),
            // Same routing IDs as Standard mode, so agents and settings keep
            // resolving across the switch.
            MuseMode::Contributor => {
                let mut routes = muse_routes();
                for route in &mut routes {
                    route.upstream_model.push_str(MUSE_CONTRIBUTOR_SUFFIX);
                }
                routes
            }
        };
        self.generated_models = grok.into_iter().chain(muse).chain(derived).collect();
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
            if let Some(window) = route.context_window {
                ensure!(
                    window > 0,
                    "context-window must be greater than zero for {}",
                    route.routing_id
                );
            }
            self.validate_muse_guards(route, cliproxy.mode)?;
        }

        self.validate_gpt_auth(cliproxy)?;
        self.validate_openai_providers(cliproxy)
    }

    fn validate_gpt_auth(&self, cliproxy: &UpstreamConfig) -> anyhow::Result<()> {
        if self.gpt.auth == GptAuth::Subscription {
            return Ok(());
        }
        ensure!(
            cliproxy.mode != UpstreamMode::External,
            "[gpt] auth = \"api-key\" requires [upstreams.{CLIPROXY_UPSTREAM}] mode = \"managed\" \
             (an external CLIProxyAPI owns its credentials; add a codex-api-key entry there \
             instead)"
        );
        if let Some(api_key) = &self.gpt.api_key {
            ensure!(
                !api_key.is_empty(),
                "[gpt] api-key in secrets.toml is empty"
            );
        }
        Ok(())
    }

    /// The Muse guards on one route: a Muse model only in the Muse family,
    /// and that family only when enabled. Also never `auto`, which can
    /// resolve to a Muse model, and a Contributor-mode model exactly when the
    /// route is a Muse one in Contributor mode, since Muse Code pairs that ID
    /// with its header. Those two are lifted only with `[muse]` off on an
    /// upstream the router configures: the managed child then serves no Meta
    /// model (`supervisor::upstream_config_yaml`) and the stub reaches none.
    /// An external instance's Meta logins and headers are its own.
    fn validate_muse_guards(
        &self,
        route: &ModelRoute,
        upstream_mode: UpstreamMode,
    ) -> anyhow::Result<()> {
        ensure!(
            route.family != ModelFamily::Muse || self.muse.enabled,
            "model {} declares family = \"muse\" but [muse] is not enabled; set [muse] \
             enabled = true",
            route.routing_id
        );
        if self.muse.enabled || upstream_mode == UpstreamMode::External {
            self.validate_muse_mode_guards(route)?;
        }
        ensure!(
            route.family == ModelFamily::Muse || !names_muse_model(&route.upstream_model),
            "model {} routes to Meta's {:?} but declares family = {:?}; set family = \
             \"muse\" so the Muse mode gate and search backend apply",
            route.routing_id,
            route.upstream_model,
            route.family.as_str()
        );
        Ok(())
    }

    /// The guards that matter whenever the upstream may serve Meta models.
    fn validate_muse_mode_guards(&self, route: &ModelRoute) -> anyhow::Result<()> {
        ensure!(
            !names_auto_model(&route.upstream_model),
            "model {} routes to {:?}, which CLIProxyAPI resolves to whichever model is \
             newest, a Muse one included; name the upstream model",
            route.routing_id,
            route.upstream_model
        );
        let contributor_model = is_muse_contributor_model(&route.upstream_model);
        if route.family == ModelFamily::Muse && self.muse.mode() == MuseMode::Contributor {
            ensure!(
                contributor_model,
                "model {} routes to {:?}, but [muse] contributor = true sends the Contributor \
                 header with every Meta request, which Muse Code pairs only with -contributor \
                 model IDs; route to the -contributor ID",
                route.routing_id,
                route.upstream_model
            );
        } else {
            ensure!(
                !contributor_model,
                "model {} routes to {:?}, a Muse Contributor-mode model, which Meta trains \
                 on; model-router sends those only from family = \"muse\" routes with [muse] \
                 contributor = true. Route to the model ID without -contributor",
                route.routing_id,
                route.upstream_model
            );
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
                // The route carries an alias; the name is what reaches the
                // host, so the Contributor guard checks it here. A plain Muse
                // name stays allowed: on Meta's pay-per-token Model API (and
                // hosts reselling it) the plain ID is Standard mode.
                // `[muse] contributor` covers the Muse Code login only: no
                // provider request carries the Contributor header.
                ensure!(
                    !is_muse_contributor_model(&model.name),
                    "openai-provider {} model {:?} is a Muse Contributor-mode model, which \
                     Meta trains on; [muse] contributor covers only the Muse Code login. Use \
                     the model ID without -contributor",
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
            r#"# model-router configuration
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
# Anthropic. By default the built-in GPT routes below are enabled; writing
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
# searches become slow and Claude Code shows "No links found."). Grok and
# Muse searches stay with their vendor even under "off"; only disabling
# the family stops them.
#[web-search]
# Default: "alpha"
#mode = "alpha"

# How the built-in GPT routes authenticate. "subscription" (the default)
# spends your ChatGPT plan through the Codex login (`model-router login`).
# "api-key" bills an OpenAI API key per token instead (managed mode only);
# any Codex login is then ignored. The key goes in secrets.toml, never here:
#   [gpt]
#   api-key = "sk-..."
#[gpt]
# Default: "subscription"
#auth = "api-key"

{grok}
{muse}
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
            providers = TEMPLATE_PROVIDERS_SECTION,
            grok = template_grok_section(),
            muse = template_muse_section(),
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
        // A key pasted without quotes is the common slip in secrets.toml.
        let hint = if error.message().contains("must be quoted") {
            " (wrap the value in double quotes)"
        } else {
            ""
        };
        anyhow::anyhow!(
            "failed to parse {}{location}: {}{hint}",
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

    /// A supported external `CLIProxyAPI` upstream, as a config fragment.
    const EXTERNAL_UPSTREAM: &str =
        "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:8317\"\n";

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
            "[muse]\nenabled = true\nnope = 1",
            // The deprecated Grok scaling key never existed for Muse.
            "[muse]\nenabled = true\ncontext-window-scaling = true",
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
    fn gpt_api_key_loads_only_under_api_key_auth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let secrets = path.with_file_name("secrets.toml");
        std::fs::write(&secrets, "[gpt]\napi-key = \"sk-live\"\n").unwrap();

        // Subscription (the default) never reads the key.
        std::fs::write(&path, "").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.gpt.auth, GptAuth::Subscription);
        assert_eq!(config.gpt.api_key, None);

        std::fs::write(&path, "[gpt]\nauth = \"api-key\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.gpt.auth, GptAuth::ApiKey);
        assert_eq!(config.gpt.api_key.as_deref(), Some("sk-live"));

        // A missing key loads (doctor reports it); an empty one is an error.
        std::fs::remove_file(&secrets).unwrap();
        assert_eq!(Config::load(&path).unwrap().gpt.api_key, None);
        std::fs::write(&secrets, "[gpt]\napi-key = \"\"\n").unwrap();
        let error = Config::load(&path).unwrap_err().to_string();
        assert!(error.contains("[gpt] api-key"), "{error}");
    }

    #[test]
    fn gpt_api_key_auth_requires_managed_mode_and_stays_out_of_the_config() {
        let error = toml::from_str::<Config>(
            "[gpt]\nauth = \"api-key\"\n\n[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:8317\"\n",
        )
        .unwrap()
        .prepare()
        .unwrap_err()
        .to_string();
        assert!(error.contains("managed"), "{error}");
        assert!(toml::from_str::<Config>("[gpt]\napi-key = \"sk-inline\"\n").is_err());
    }

    #[test]
    fn unquoted_secret_gets_a_quoting_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gpt]\nauth = \"api-key\"\n").unwrap();
        std::fs::write(
            path.with_file_name("secrets.toml"),
            "[gpt]\napi-key = sk-SENTINEL-DO-NOT-PRINT\n",
        )
        .unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(error.contains("double quotes"), "{error}");
        assert!(!error.contains("SENTINEL"), "{error}");
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
        let error = prepare_error(&kimi_toml("min-context-window = 0"));
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

    /// One `OpenRouter` model with the context fields under test.
    fn kimi_toml(window: &str) -> String {
        format!(
            r#"
                [[openai-providers]]
                name = "openrouter"
                base-url = "https://openrouter.ai/api/v1"
                [[openai-providers.models]]
                name = "moonshotai/kimi-k3"
                routing-id = "kimi-k3"
                display-name = "Kimi K3"
                {window}
            "#
        )
    }

    fn route<'a>(config: &'a Config, routing_id: &str) -> &'a ModelRoute {
        config
            .effective_models()
            .find(|route| route.routing_id == routing_id)
            .unwrap()
    }

    /// [`kimi_toml`] with the deprecated scaling on, under the declaration
    /// the old setup wrote.
    fn scaled_kimi(window: u64) -> String {
        format!(
            "declared-context-window = {CODEX_DEFAULT_CONTEXT_WINDOW}\n{}",
            kimi_toml(&format!(
                "context-window = {window}\ncontext-window-scaling = true"
            ))
        )
    }

    #[test]
    fn derived_route_inherits_window_and_scale() {
        let config = parse_and_prepare(&kimi_toml("context-window = 1000000"));
        let unscaled = route(&config, "kimi-k3");
        assert_eq!(unscaled.context_window, Some(1_000_000));
        assert!(unscaled.usage_scale.is_none());

        let config = parse_and_prepare(&scaled_kimi(1_000_000));
        let route = route(&config, "kimi-k3");
        assert!(route.context_window_scaling);
        // A real 1M-token conversation reports as the declared window, so
        // Claude Code compacts at the model's real limit.
        assert_eq!(
            route.usage_scale.unwrap().apply(1_000_000),
            CODEX_DEFAULT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn scaling_is_none_when_the_windows_already_agree() {
        let config = parse_and_prepare(&scaled_kimi(CODEX_DEFAULT_CONTEXT_WINDOW));
        assert!(route(&config, "kimi-k3").usage_scale.is_none());
    }

    #[test]
    fn scaling_a_window_below_the_declared_one_leaves_the_route_unscaled() {
        // Scaling up is unsound, but a config that asks for it must still
        // serve: the service logs it and doctor judges the route unscaled.
        let config = parse_and_prepare(&scaled_kimi(125_000));
        let route = route(&config, "kimi-k3");
        assert_eq!(route.context_window, Some(125_000));
        assert!(route.usage_scale.is_none());
    }

    #[test]
    fn a_scaling_route_without_a_window_waits_for_discovery() {
        // `serve` fills these in from the host; until then the route is
        // simply unscaled rather than a config error.
        let config = parse_and_prepare(&kimi_toml("context-window-scaling = true"));
        let route = route(&config, "kimi-k3");
        assert!(route.context_window.is_none());
        assert!(route.usage_scale.is_none());
    }

    #[test]
    fn the_clients_window_is_adopted_unless_the_config_declares_one() {
        let source = kimi_toml("context-window = 1000000\ncontext-window-scaling = true");
        let scaled = |config: &Config| {
            route(config, "kimi-k3")
                .usage_scale
                .unwrap()
                .apply(1_000_000)
        };

        let mut config = parse_and_prepare(&source);
        config.adopt_client_window(EnvSetting::Settings(500_000));
        assert_eq!(config.declared_context_window, Some(500_000));
        assert_eq!(scaled(&config), 500_000);
        // Re-preparing (discovery does) keeps the adopted declaration.
        config.prepare().unwrap();
        assert_eq!(scaled(&config), 500_000);

        // A raised declaration the route's window does not exceed: unscaled.
        let mut config = parse_and_prepare(&source);
        config.adopt_client_window(EnvSetting::Environment(1_000_000));
        assert!(route(&config, "kimi-k3").usage_scale.is_none());

        // Nothing resolved: Claude Code's own default.
        let mut config = parse_and_prepare(&source);
        config.adopt_client_window(EnvSetting::Unresolved);
        assert_eq!(
            scaled(&config),
            crate::client_window::UNDECLARED_CONTEXT_WINDOW
        );

        let mut config = parse_and_prepare(&scaled_kimi(1_000_000));
        config.adopt_client_window(EnvSetting::Settings(500_000));
        assert_eq!(scaled(&config), CODEX_DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn overflow_translation_is_armed_for_built_in_models_only() {
        use crate::overflow::OverflowDialect;
        let config = parse_and_prepare(&format!(
            "{}[grok]\nenabled = true\n[muse]\nenabled = true\n",
            kimi_toml("")
        ));
        let dialect = |routing_id: &str| overflow_dialect(route(&config, routing_id));
        assert_eq!(dialect("gpt-5.6-sol"), Some(OverflowDialect::Codex));
        assert_eq!(dialect("grok-4.5"), Some(OverflowDialect::Xai));
        assert_eq!(dialect("muse-wonder-1.0"), Some(OverflowDialect::Meta));
        assert_eq!(dialect("muse-wonder-full-1.0"), Some(OverflowDialect::Meta));
        assert_eq!(dialect("kimi-k3"), None);
        // A hand-written route inherits the family but not the verified backend.
        let hand_written = parse_and_prepare(
            "[[models]]\nrouting-id = \"a\"\nupstream-model = \"m\"\ndisplay-name = \"A\"\n\n\
             [[models]]\nrouting-id = \"b\"\nupstream-model = \"m2\"\ndisplay-name = \"B\"\nfamily = \"grok\"\n",
        );
        assert_eq!(overflow_dialect(&hand_written.models[0]), None);
        assert_eq!(overflow_dialect(&hand_written.models[1]), None);
        // A pinned effort suffix still names the built-in model.
        let pinned = parse_and_prepare(
            "[[models]]\nrouting-id = \"g\"\nupstream-model = \"grok-4.5(low)\"\n\
             display-name = \"G\"\nfamily = \"grok\"\n",
        );
        assert_eq!(
            overflow_dialect(&pinned.models[0]),
            Some(OverflowDialect::Xai)
        );
    }

    #[test]
    fn windows_must_be_positive() {
        assert!(prepare_error(&kimi_toml("context-window = 0")).contains("greater than zero"));
        assert!(prepare_error("declared-context-window = 0").contains("must be greater than zero"));
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

        let config = parse_and_prepare(
            "declared-context-window = 258400\n[grok]\nenabled = true\ncontext-window-scaling = \
             true\n",
        );
        let route = route(&config, "grok-4.5");
        let scale = route.usage_scale.expect("grok-4.5 unscaled");
        // A full real window reports as the declared one.
        assert_eq!(
            scale.apply(route.context_window.unwrap()),
            CODEX_DEFAULT_CONTEXT_WINDOW
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

    // ---- Muse family (optional) ----

    #[test]
    fn muse_routes_exist_only_when_enabled_and_survive_a_hand_written_models_block() {
        for source in ["", "[muse]\nenabled = false\n", "[grok]\nenabled = true\n"] {
            let config = parse_and_prepare(source);
            assert!(
                !config
                    .effective_models()
                    .any(|r| r.family == ModelFamily::Muse),
                "{source:?}"
            );
        }
        let config = parse_and_prepare(
            "[[models]]\nrouting-id = \"mine\"\nupstream-model = \"m\"\ndisplay-name = \"M\"\n\n[muse]\nenabled = true\n",
        );
        assert_eq!(config.models.len(), 1);
        for (model, display_name) in MUSE_MODELS {
            let route = route(&config, model);
            assert_eq!(route.family, ModelFamily::Muse);
            assert_eq!(route.upstream_model, model);
            assert_eq!(route.display_name, display_name);
            assert_eq!(route.context_window, Some(MUSE_CONTEXT_WINDOW));
            assert!(!route.context_window_scaling);
            assert!(route.usage_scale.is_none());
        }
        // Both families at once, each with its own routes.
        let both = parse_and_prepare("[grok]\nenabled = true\n[muse]\nenabled = true\n");
        assert_eq!(
            both.generated_models.len(),
            GROK_MODELS.len() + MUSE_MODELS.len()
        );
    }

    #[test]
    fn muse_family_is_readable_from_toml_and_carried_by_generated_routes() {
        let config = parse_and_prepare(
            "[muse]\nenabled = true\n\n[[models]]\nrouting-id = \"b\"\nupstream-model = \"m2\"\n\
             display-name = \"B\"\nfamily = \"muse\"\n",
        );
        assert_eq!(config.models[0].family, ModelFamily::Muse);
        assert_eq!(route(&config, "muse-wonder-1.0").family, ModelFamily::Muse);
        assert_eq!(ModelFamily::Muse.as_str(), "muse");
    }

    #[test]
    fn only_grok_takes_effort_in_the_model_id() {
        for (family, suffix) in [
            (ModelFamily::Gpt, false),
            (ModelFamily::Grok, true),
            (ModelFamily::Muse, false),
            (ModelFamily::OpenAiCompat, false),
        ] {
            assert_eq!(family.effort_via_model_suffix(), suffix, "{family:?}");
        }
    }

    #[test]
    fn built_in_muse_routes_never_name_a_contributor_model() {
        for (model, _) in MUSE_MODELS {
            assert!(!is_muse_contributor_model(model), "{model}");
            assert!(names_muse_model(model), "{model}");
        }
    }

    #[test]
    fn the_contributor_predicate_sees_through_effort_prefix_and_case() {
        for model in [
            "muse-wonder-1.0-contributor",
            "muse-wonder-full-1.0-contributor",
            "muse-spark-1.3-contributor",
            "muse-wonder-1.0-contributor(high)",
            "meta/muse-wonder-1.0-contributor",
            "Muse-Wonder-1.0-CONTRIBUTOR",
        ] {
            assert!(is_muse_contributor_model(model), "{model}");
        }
        for model in [
            "muse-wonder-1.0",
            "muse-wonder-1.0(max)",
            "muse-wonder-contributor-edition-1.0",
            "gpt-6-astra",
        ] {
            assert!(!is_muse_contributor_model(model), "{model}");
        }
    }

    #[test]
    fn a_contributor_route_fails_to_load_in_standard_mode_whatever_family_it_declares() {
        for family in ["muse", "gpt", "grok"] {
            for upstream in [
                "muse-wonder-1.0-contributor",
                "muse-wonder-full-1.0-contributor(low)",
                "renamed-1.0-contributor",
            ] {
                let source = format!(
                    "[muse]\nenabled = true\n[[models]]\nrouting-id = \"m\"\n\
                     upstream-model = \"{upstream}\"\ndisplay-name = \"M\"\n\
                     family = \"{family}\"\n"
                );
                let error = prepare_error(&source);
                assert!(
                    error.contains("Contributor-mode model"),
                    "{family} {upstream}: {error}"
                );
            }
        }
        // With Muse off, a Muse-named route fails on its family, and nothing
        // else names Meta's models: the child serves none then.
        let error = prepare_error(
            "[[models]]\nrouting-id = \"m\"\nupstream-model = \"muse-wonder-1.0-contributor\"\n\
             display-name = \"M\"\n",
        );
        assert!(error.contains("family = \"muse\""), "{error}");
        let renamed = "[[models]]\nrouting-id = \"m\"\nupstream-model = \
                       \"renamed-1.0-contributor\"\ndisplay-name = \"M\"\n";
        parse_and_prepare(renamed);
        // An external instance may serve Meta with Muse off, so it keeps
        // the guard.
        let error = prepare_error(&format!("{EXTERNAL_UPSTREAM}{renamed}"));
        assert!(error.contains("Contributor-mode model"), "{error}");
        // Through `Config::load`, the path `serve` and `login` take.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[muse]\nenabled = true\n\n[[models]]\nrouting-id = \"m\"\n\
             upstream-model = \"muse-wonder-1.0-contributor\"\ndisplay-name = \"M\"\n\
             family = \"muse\"\n",
        )
        .unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(error.contains("Contributor-mode model"), "{error}");
    }

    #[test]
    fn contributor_mode_moves_the_built_in_muse_routes_to_their_contributor_ids() {
        let config = parse_and_prepare("[muse]\nenabled = true\ncontributor = true\n");
        assert_eq!(config.muse.mode(), MuseMode::Contributor);
        for (model, display_name) in MUSE_MODELS {
            let route = route(&config, model);
            assert_eq!(route.upstream_model, format!("{model}-contributor"));
            assert_eq!(route.display_name, display_name);
            assert_eq!(route.family, ModelFamily::Muse);
            assert_eq!(route.context_window, Some(MUSE_CONTEXT_WINDOW));
            assert_eq!(
                overflow_dialect(route),
                Some(crate::overflow::OverflowDialect::Meta)
            );
        }
        // Switching back restores the plain IDs; `contributor` alone is inert.
        let config = parse_and_prepare("[muse]\nenabled = true\ncontributor = false\n");
        assert_eq!(config.muse.mode(), MuseMode::Standard);
        assert_eq!(
            route(&config, "muse-wonder-1.0").upstream_model,
            "muse-wonder-1.0"
        );
        let config = parse_and_prepare("[muse]\ncontributor = true\n");
        assert_eq!(config.muse.mode(), MuseMode::Off);
        assert!(
            !config
                .effective_models()
                .any(|r| r.family == ModelFamily::Muse)
        );
        assert!(names_muse_wonder_full(
            "muse-wonder-full-1.0-contributor(high)"
        ));
        assert!(!names_muse_wonder_full("muse-wonder-1.0-contributor"));
    }

    #[test]
    fn contributor_mode_accepts_contributor_ids_on_muse_routes_and_nothing_else() {
        let contributor = "[muse]\nenabled = true\ncontributor = true\n";
        let route = |upstream: &str, family: &str| {
            format!(
                "{contributor}[[models]]\nrouting-id = \"m\"\nupstream-model = \"{upstream}\"\n\
                 display-name = \"M\"\nfamily = \"{family}\"\n"
            )
        };
        let config = parse_and_prepare(&route("muse-wonder-1.0-contributor(high)", "muse"));
        assert_eq!(
            config.models[0].upstream_model,
            "muse-wonder-1.0-contributor(high)"
        );
        // A plain ID would pair the header with a shape Muse Code never sends.
        let error = prepare_error(&route("muse-wonder-1.0", "muse"));
        assert!(error.contains("route to the -contributor ID"), "{error}");
        // Other families never carry a Contributor ID.
        for family in ["gpt", "grok"] {
            let error = prepare_error(&route("renamed-1.0-contributor", family));
            assert!(
                error.contains("Contributor-mode model"),
                "{family}: {error}"
            );
        }
        // Nor does an openai-compat provider: the header rides the Meta login.
        let error = prepare_error(&format!(
            "{contributor}[[openai-providers]]\nname = \"meta\"\n\
             base-url = \"https://api.example.com/v1\"\n[[openai-providers.models]]\n\
             name = \"muse-wonder-1.0-contributor\"\nrouting-id = \"w\"\ndisplay-name = \"W\"\n"
        ));
        assert!(error.contains("Contributor-mode model"), "{error}");
    }

    #[test]
    fn compaction_forces_text_only_on_codex_native_gpt_and_on_grok() {
        let forced = |upstream_model: &str, family| {
            forces_text_on_compaction(&ModelRoute {
                upstream_model: upstream_model.to_string(),
                family,
                ..ModelRoute::default()
            })
        };
        assert!(forced("gpt-6-astra", ModelFamily::Gpt));
        assert!(forced("gpt-6.1-sol(high)", ModelFamily::Gpt));
        assert!(!forced("kimi-k3", ModelFamily::Gpt));
        assert!(!forced("openai-compat--kimi-k3", ModelFamily::OpenAiCompat));
        assert!(!forced("gpt-6-astra", ModelFamily::OpenAiCompat));
        assert!(forced("grok-4.7", ModelFamily::Grok));
        assert!(forced("grok-4.20-x", ModelFamily::Grok));
        assert!(!forced("muse-wonder-1.0", ModelFamily::Muse));
    }

    #[test]
    fn a_route_to_a_muse_model_must_declare_the_muse_family() {
        for family in ["", "family = \"gpt\"\n", "family = \"grok\"\n"] {
            let source = format!(
                "[[models]]\nrouting-id = \"m\"\nupstream-model = \"muse-wonder-1.0(high)\"\n\
                 display-name = \"M\"\n{family}"
            );
            let error = prepare_error(&source);
            assert!(error.contains("family = \"muse\""), "{family:?}: {error}");
        }
        let muse_route = "[[models]]\nrouting-id = \"m\"\nupstream-model = \"muse-wonder-1.0(high)\"\n\
             display-name = \"M\"\nfamily = \"muse\"\n";
        parse_and_prepare(&format!("[muse]\nenabled = true\n{muse_route}"));
        // The family is the Muse switch's to turn on: `login muse` and the
        // doctor's auth check follow `[muse] enabled`.
        let error = prepare_error(muse_route);
        assert!(error.contains("[muse] is not enabled"), "{error}");
    }

    #[test]
    fn the_contributor_predicate_sees_through_the_whitespace_the_child_trims() {
        for model in [
            "muse-wonder-1.0-contributor ",
            " muse-wonder-1.0-contributor(high)",
            "muse-wonder-1.0-contributor\n",
            "\tmuse-wonder-1.0-contributor (low)",
        ] {
            assert!(is_muse_contributor_model(model), "{model:?}");
        }
        let error = prepare_error(
            "[muse]\nenabled = true\n[[models]]\nrouting-id = \"m\"\n\
             upstream-model = \"muse-wonder-1.0-contributor \"\ndisplay-name = \"M\"\n\
             family = \"muse\"\n",
        );
        assert!(error.contains("Contributor-mode model"), "{error}");
    }

    #[test]
    fn a_route_to_auto_fails_to_load_unless_meta_is_excluded() {
        // The child resolves `auto` to its newest model: with a Meta login,
        // a Muse model behind a route that is not in the Muse family.
        for upstream in ["auto", "auto(high)", " AUTO "] {
            let route = format!(
                "[[models]]\nrouting-id = \"m\"\nupstream-model = \"{upstream}\"\n\
                 display-name = \"M\"\n"
            );
            for muse in [
                "[muse]\nenabled = true\n",
                "[muse]\nenabled = true\ncontributor = true\n",
            ] {
                let error = prepare_error(&format!("{muse}{route}"));
                assert!(
                    error.contains("name the upstream model"),
                    "{upstream}: {error}"
                );
            }
            // With Muse off the managed child serves no Meta model, so a
            // route that loaded before the Muse family existed still loads.
            for muse in ["", "[muse]\nenabled = false\ncontributor = true\n"] {
                let config = parse_and_prepare(&format!("{muse}{route}"));
                assert_eq!(config.models[0].upstream_model, upstream);
            }
            // An external instance's Meta logins are its own: Muse off
            // excludes nothing there.
            for muse in ["", "[muse]\nenabled = false\n"] {
                let error = prepare_error(&format!("{muse}{EXTERNAL_UPSTREAM}{route}"));
                assert!(
                    error.contains("name the upstream model"),
                    "{upstream}: {error}"
                );
            }
        }
        // Another host's model that happens to be called auto is not this one.
        assert!(!names_auto_model("openrouter/auto"));
    }

    #[test]
    fn a_provider_contributor_model_fails_to_load_whatever_its_routing_id() {
        let source = |name: &str| {
            format!(
                "[[openai-providers]]\nname = \"meta\"\nbase-url = \"https://api.example.com/v1\"\n\
                 [[openai-providers.models]]\nname = \"{name}\"\nrouting-id = \"wonder\"\n\
                 display-name = \"Wonder\"\n"
            )
        };
        for name in [
            "muse-wonder-1.0-contributor",
            "meta/Muse-Wonder-1.0-Contributor",
        ] {
            let error = prepare_error(&source(name));
            assert!(error.contains("Contributor-mode model"), "{name}: {error}");
        }
        // Meta's pay-per-token Model API serves the plain ID in Standard mode.
        for name in ["muse-wonder-1.0", "meta/muse-wonder"] {
            parse_and_prepare(&source(name));
        }
    }

    #[test]
    fn built_in_routes_of_every_family_are_known_without_a_config() {
        for model in [
            "gpt-6-astra",
            "gpt-5.6-luna",
            "grok-4.7",
            "muse-wonder-1.0",
            "muse-wonder-full-1.0",
            "muse-wonder-1.0(high)",
        ] {
            assert!(is_built_in_route(model), "{model}");
        }
        for model in ["opus", "kimi-k3", "muse-wonder-1.0-contributor", ""] {
            assert!(!is_built_in_route(model), "{model}");
        }
    }

    #[test]
    fn the_template_documents_muse_off_by_default() {
        let template = Config::template();
        assert!(template.contains("#[muse]\n# Default: false\n#enabled = true\n"));
        assert!(template.contains("Default: false\n#contributor = true\n"));
        // The commented section, uncommented, is a valid config.
        let start = template.find("#[muse]").unwrap();
        let section: String = template[start..]
            .lines()
            .take_while(|line| line.starts_with('#'))
            .filter(|line| !line.starts_with("# "))
            .map(|line| &line[1..])
            .collect::<Vec<_>>()
            .join("\n");
        let config = parse_and_prepare(&section);
        assert_eq!(config.muse.mode(), MuseMode::Contributor, "{section}");
        for (model, _) in MUSE_MODELS {
            assert!(
                template.contains(&format!("#   {model} ({MUSE_CONTEXT_WINDOW} tokens)")),
                "{model}"
            );
        }
        assert!(!template.contains("-contributor ("), "{template}");
    }
}
