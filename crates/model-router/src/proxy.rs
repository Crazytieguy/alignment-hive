//! The gateway's HTTP front: the ingress gate, then each request down one
//! of two branches. The Claude branch forwards to Anthropic byte-exact,
//! credentials and all; the GPT branch rewrites the body for the matched
//! route, strips Claude credentials, and sends it to the `CLIProxyAPI`
//! upstream (or the built-in stub). Responses get the `WebSearch` tap, usage
//! rewriting and overflow translation (GPT only), and capture. `WebSearch`
//! sub-calls are answered in [`search`].

use std::convert::Infallible;
use std::future::{Future, IntoFuture as _};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::Response;
use axum::{Router, routing::any};
use futures_util::StreamExt;
use serde_json::json;
use tokio::net::TcpListener;

use crate::capture::{CaptureSink, RequestCapture, StreamingCapture, redact_headers};
use crate::config::{Config, ModelFamily, MuseMode, UpstreamMode, WebSearchMode};
use crate::headers;
use crate::overflow::OverflowRewrite;
use crate::routing::{RoutingDecision, decide, substitute_model};
use crate::stub;
use crate::usage::{GptPolicies, SseUsageTransformer, estimate_input_tokens};
use crate::websearch;

mod search;

pub use search::SearchStreams;

/// Maximum time SIGINT/SIGTERM may spend draining in-flight connections.
/// Leaves enough of the service manager's 60-second stop budget for managed
/// child teardown.
pub const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(50);

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    client: reqwest::Client,
    capture: Option<CaptureSink>,
    cliproxy_upstream: CliproxyUpstream,
    /// `WebSearch` invocations observed in model responses, awaiting their
    /// side calls (origin-matched backend routing).
    pending_searches: Arc<websearch::PendingSearches>,
    /// In-flight vendor-native search streams, owned so shutdown can wait
    /// for them.
    search_streams: Arc<SearchStreams>,
    /// The logins seen undoing a model exclusion, or setting the Muse
    /// Contributor header, since startup ([`login_refusal`]).
    seen_overrides: Arc<crate::exclusions::Seen>,
    seen_headers: Arc<crate::muse::SeenHeaders>,
}

/// Passive response tap: watches a forwarded `/v1/messages` response for
/// `WebSearch` `tool_use` blocks and records who asked, so the follow-up
/// sub-call can be routed to the matching backend.
struct SniffTap {
    pending: Arc<websearch::PendingSearches>,
    session_id: String,
    origin: websearch::Origin,
}

#[derive(Clone)]
enum CliproxyUpstream {
    Stub,
    External {
        base_url: String,
        credential: Option<headers::GptUpstreamCredential>,
    },
    Managed(crate::supervisor::ManagedHandle),
    /// Managed mode without a running supervisor (startup failed): Claude
    /// traffic is unaffected, GPT requests get an actionable error.
    ManagedUnavailable,
}

impl CliproxyUpstream {
    fn resolve(
        config: &Config,
        managed: Option<crate::supervisor::ManagedHandle>,
    ) -> anyhow::Result<Self> {
        let upstream = config.cliproxy_upstream();
        match upstream.mode {
            UpstreamMode::Stub => Ok(Self::Stub),
            UpstreamMode::External => Ok(Self::External {
                base_url: upstream
                    .base_url
                    .clone()
                    .expect("validated external upstream always has a base URL"),
                credential: upstream
                    .api_key
                    .as_deref()
                    .map(headers::GptUpstreamCredential::new)
                    .transpose()?
                    .map(|credential| credential.for_muse_mode(config.muse.mode())),
            }),
            // The handle's credential carries the mode from `Supervisor::start`.
            UpstreamMode::Managed => Ok(managed.map_or(Self::ManagedUnavailable, Self::Managed)),
        }
    }
}

impl AppState {
    async fn new(
        mut config: Config,
        managed: Option<crate::supervisor::ManagedHandle>,
    ) -> anyhow::Result<Self> {
        config.prepare()?;
        let cliproxy_upstream = CliproxyUpstream::resolve(&config, managed)?;
        let capture = if config.capture.enabled {
            Some(
                CaptureSink::open(&config.capture.file, config.capture.max_response_body_bytes)
                    .await?,
            )
        } else {
            None
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            // Idle-read timeout: resets on every received chunk, so long SSE
            // streams are fine while a blackholed upstream still times out.
            .read_timeout(std::time::Duration::from_mins(10))
            .build()?;
        let search_streams = Arc::new(SearchStreams::new());
        // What the child is about to load: an override or a header there may
        // outlive a fix made before the first request that relies on it.
        let seen_overrides = Arc::new(crate::exclusions::Seen::default());
        let seen_headers = Arc::new(crate::muse::SeenHeaders::default());
        if let Some(auth_dir) = &config.auth_dir {
            let mode = config.muse.mode();
            let logins = crate::logins::walk(auth_dir);
            let required = crate::exclusions::required(mode, config.gpt.auth);
            crate::exclusions::record(&seen_overrides, &logins, &required);
            crate::muse::record(&seen_headers, &logins, mode);
        }
        Ok(Self {
            config: Arc::new(config),
            client,
            capture,
            cliproxy_upstream,
            pending_searches: Arc::new(websearch::PendingSearches::default()),
            search_streams,
            seen_overrides,
            seen_headers,
        })
    }

    /// Builds the response tap for a `/v1/messages` request when searches
    /// from `origin` are intercepted ([`Self::intercepts`]) and the request
    /// declares the client `WebSearch` tool (cheap byte prefilter; a false
    /// positive merely arms the passive tap).
    fn websearch_tap(&self, body: &[u8], origin: websearch::Origin) -> Option<SniffTap> {
        if !self.intercepts(&origin) || !websearch::declares_websearch_tool(body) {
            return None;
        }
        Some(SniffTap {
            pending: self.pending_searches.clone(),
            session_id: websearch::session_id(body)?,
            origin,
        })
    }

    /// Whether `WebSearch` calls from `origin` are intercepted: whenever
    /// `[web-search] mode` is not `off`, and always for a routed model whose
    /// searches go to its own vendor's search (Grok, Muse). Those never
    /// leave the vendor: `off` governs only the other families.
    fn intercepts(&self, origin: &websearch::Origin) -> bool {
        self.config.web_search.mode != WebSearchMode::Off
            || matches!(origin, websearch::Origin::Gpt { routing_id }
                if self.config.route(routing_id)
                    .is_some_and(|route| websearch::native_search(route.family).is_some()))
    }

    /// The recorded origin of a `WebSearch` sub-call, if its session's
    /// `tool_use` was observed.
    fn origin_of(&self, body: &[u8], subcall: &websearch::Subcall) -> Option<websearch::Origin> {
        let session_id = websearch::session_id(body)?;
        self.pending_searches
            .consume(&websearch::PendingKey::for_subcall(session_id, subcall))
    }
}

/// The unauthenticated introspection endpoint: exempt from the ingress gate
/// and matched by the handler. One constant so the two can never drift.
pub const HEALTH_PATH: &str = "/__model-router/health";

/// The tokened path prefix the ingress gate accepts.
fn ingress_prefix(token: &str) -> String {
    format!("/t/{token}")
}

/// The full gateway base URL for a given bind address and ingress token —
/// the value Claude Code uses as `ANTHROPIC_BASE_URL`.
#[must_use]
pub fn tokened_base_url(address: &SocketAddr, token: &str) -> String {
    format!("http://{address}{}", ingress_prefix(token))
}

/// Serves using an already-bound loopback listener.
///
/// # Errors
/// Returns an error for non-loopback listeners, invalid configuration, or
/// server failures.
pub async fn serve_listener(
    listener: TcpListener,
    config: Config,
    managed: Option<crate::supervisor::ManagedHandle>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_listener_with_drain(listener, config, managed, shutdown, SHUTDOWN_DRAIN_TIMEOUT).await
}

/// Serves with an explicit shutdown drain bound (primarily for tests).
///
/// # Errors
/// Returns an error for non-loopback listeners, invalid configuration, or
/// server failures.
pub async fn serve_listener_with_drain(
    listener: TcpListener,
    config: Config,
    managed: Option<crate::supervisor::ManagedHandle>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    drain_timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let address = listener.local_addr()?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "refusing non-loopback listener address {address}"
    );
    let (app, searches) = build(config, managed).await?;
    serve_app(listener, app, searches, shutdown, drain_timeout).await?;
    Ok(())
}

async fn serve_app(
    listener: TcpListener,
    app: Router,
    searches: Arc<SearchStreams>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    drain_timeout: std::time::Duration,
) -> std::io::Result<()> {
    let (graceful_tx, graceful_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = graceful_rx.await;
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result,
        () = shutdown => {
            // Admission closes first, before connections drain: a search
            // arriving mid-drain must fail visibly rather than open a stream
            // that nothing is left to finish.
            searches.close();
            let _ = graceful_tx.send(());
            let started = std::time::Instant::now();
            let result = if let Ok(result) = tokio::time::timeout(drain_timeout, &mut server).await {
                result
            } else {
                tracing::warn!(
                    timeout_seconds = drain_timeout.as_secs_f64(),
                    "shutdown drain expired; dropping in-flight connections"
                );
                Ok(())
            };
            // Search streams outlive their requests, so the already-admitted
            // ones are waited for here — with whatever budget the connection
            // drain left, and before the caller tears the managed child down
            // (see `SearchStreams`).
            searches
                .wait(drain_timeout.saturating_sub(started.elapsed()))
                .await;
            result
        }
    }
}

/// Builds the gateway service without binding a socket (stub/external only).
///
/// # Errors
/// Returns an error for invalid configuration, capture-file failures, or HTTP
/// client initialization failures.
pub async fn app(config: Config) -> anyhow::Result<Router> {
    Ok(build(config, None).await?.0)
}

/// The router plus the handle shutdown needs to wait for in-flight search
/// streams.
async fn build(
    config: Config,
    managed: Option<crate::supervisor::ManagedHandle>,
) -> anyhow::Result<(Router, Arc<SearchStreams>)> {
    let state = AppState::new(config, managed).await?;
    let searches = Arc::clone(&state.search_streams);
    Ok((
        Router::new().fallback(any(handle)).with_state(state),
        searches,
    ))
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();

    if !apply_ingress_gate(&state.config, &mut parts) {
        // Generic: no token material.
        return local_error_response(
            &state,
            StatusCode::NOT_FOUND,
            "not_found_error",
            "not found",
            None,
        )
        .await;
    }

    let body = match to_bytes(body, state.config.max_request_body_bytes).await {
        Ok(body) => body,
        Err(error) => {
            let over_limit = error.to_string().contains("length limit exceeded");
            tracing::warn!(%error, over_limit, "failed to read inbound request body");
            let (status, message) = if over_limit {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds max-request-body-bytes",
                )
            } else {
                (StatusCode::BAD_REQUEST, "failed to read request body")
            };
            return local_error_response(&state, status, "invalid_request_error", message, None)
                .await;
        }
    };

    if parts.method == Method::GET && parts.uri.path() == HEALTH_PATH {
        return health_response(&state);
    }

    let decision = decide(&state.config, &body);
    let capture = state.capture.as_ref().map(|_| RequestCapture {
        branch: if decision.route.is_some() {
            "gpt"
        } else {
            "claude"
        },
        family: decision.family_label().to_string(),
        model: decision.model.clone(),
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(ToOwned::to_owned),
        headers: redact_headers(&parts.headers),
        body: body.to_vec(),
    });

    tracing::info!(
        method = %parts.method,
        path = %parts.uri.path(),
        family = decision.family_label(),
        model = decision.model.as_deref().unwrap_or("<none>"),
        "routing request"
    );

    if parts.method == Method::POST
        && parts.uri.path() == "/v1/messages/count_tokens"
        && decision.route.is_some()
    {
        return local_error_response(
            &state,
            StatusCode::NOT_FOUND,
            "not_found_error",
            "token counting is not available for routed GPT models",
            capture,
        )
        .await;
    }

    // A route that asked for sub-provider pinning and has no selection is
    // not in the child config either; answering here keeps the reason
    // attached to the failure instead of the child's generic 400.
    if let Some(route) = decision.route
        && let Some(wanted) = route.unserved_min_window()
    {
        return local_error_response(
            &state,
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!(
                "{} is not served: it wants sub-providers serving at least {wanted} tokens and \
                 the service's last lookup found none that qualify; lower min-context-window or \
                 run `model-router verify-providers`, then `model-router service restart`",
                route.routing_id
            ),
            capture,
        )
        .await;
    }

    if let Some(message) = login_refusal(&state, decision.route).await {
        return local_error_response(
            &state,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            &message,
            capture,
        )
        .await;
    }

    match decision.route {
        None => claude_response(&state, &parts, body, &decision, capture).await,
        Some(route) => gpt_response(&state, &parts, body, route, capture).await,
    }
}

/// The client-facing refusal for a request to `route` that must not be
/// forwarded now, from one walk of the managed child's auth dir per request,
/// off the async workers: a login undoes (or once undid, see
/// [`crate::exclusions::Seen`]) a model exclusion the route relies on, or,
/// for a Muse route, the Muse gate refuses ([`crate::muse::refusal`] for the
/// configured mode, with the headers seen since startup). Without a managed
/// auth dir only the Muse gate applies: an external child's logins are its
/// own.
async fn login_refusal(
    state: &AppState,
    route: Option<&crate::config::ModelRoute>,
) -> Option<String> {
    let route = route?;
    let auth_dir = state.config.auth_dir.clone();
    let mode = state.config.muse.mode();
    let required = crate::exclusions::required(mode, state.config.gpt.auth);
    let relied_on = crate::exclusions::relied_on(&required, route);
    let muse = route.family == ModelFamily::Muse;
    if !muse && (relied_on.is_empty() || auth_dir.is_none()) {
        return None;
    }
    let seen_overrides = state.seen_overrides.clone();
    let seen_headers = state.seen_headers.clone();
    let refused = tokio::task::spawn_blocking(move || {
        let logins = auth_dir.as_deref().map(crate::logins::walk);
        if let Some(reason) = logins.as_ref().and_then(|logins| {
            crate::exclusions::refusal(&seen_overrides, logins, &required, &relied_on)
        }) {
            return Some(format!("model-router refused this request: {reason}"));
        }
        muse.then(|| crate::muse::refusal(logins.as_ref(), mode, &seen_headers))
            .flatten()
            .map(|reason| muse_message(mode, &reason))
    })
    .await
    .unwrap_or_else(|error| Some(format!("the login check failed ({error})")));
    if let Some(message) = &refused {
        tracing::warn!(route = %route.routing_id, %message, "refused a request over the logins");
    }
    refused
}

/// The client-facing text for a request the Muse gate refuses in `mode`.
fn muse_message(mode: MuseMode, reason: &str) -> String {
    if mode == MuseMode::Contributor {
        format!(
            "model-router refused this Muse request in Contributor mode ([muse] \
             contributor = true): {reason}"
        )
    } else {
        format!(
            "model-router refused this Muse request so that Meta never receives \
             Contributor-mode traffic: {reason}"
        )
    }
}

async fn claude_response(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    decision: &RoutingDecision<'_>,
    capture: Option<RequestCapture>,
) -> Response {
    let is_messages_post = parts.method == Method::POST && parts.uri.path() == "/v1/messages";
    let mut tap = None;
    if is_messages_post {
        if let Some(subcall) = websearch::detect(&body) {
            // Only an intercepted origin was recorded (see `websearch_tap`);
            // the check keeps that explicit.
            if let Some(origin) = state.origin_of(&body, &subcall)
                && state.intercepts(&origin)
                && let websearch::Origin::Gpt { routing_id } = origin
            {
                return search::gpt_origin_on_claude_branch(
                    state,
                    parts,
                    body,
                    &routing_id,
                    &subcall,
                    capture,
                )
                .await;
            }
            // Claude origin or no observation: native passthrough below.
        } else if let Some(model) = decision.model.clone() {
            tap = state.websearch_tap(&body, websearch::Origin::Claude { model });
        }
    }
    forward_to_anthropic(state, parts, body, capture, tap).await
}

/// Forwards a Claude-branch request byte-exact, credentials and all.
async fn forward_to_anthropic(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    capture: Option<RequestCapture>,
    tap: Option<SniffTap>,
) -> Response {
    forward(
        state,
        &state.config.anthropic_upstream_base,
        &parts.method,
        &parts.uri,
        &parts.headers,
        body,
        false,
        false,
        None,
        None,
        capture,
        tap,
    )
    .await
}

/// The gateway hint Claude Code puts on its compaction requests (sent with
/// `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` or the first-party base-URL flag).
const COMPACTION_HEADER: &str = "x-claude-code-compaction";

/// The DOM-level rewrites every GPT-branch turn gets, in one parse: the
/// identity system block ([`crate::identity`]), the removal of tool-schema
/// patterns the Codex validator cannot compile ([`crate::tool_schema`]), and
/// on a compaction request a text-only reply.
///
/// Compaction is a one-turn fork that keeps the conversation's tools (for the
/// prompt cache) and asks for a text summary. A GPT subagent answers it by
/// calling its hand-back tool instead, which Claude Code reads as an empty
/// summary, so the compaction fails and the agent runs on to the hard limit.
/// `tool_choice: none` keeps the tools declared and forces text, on the
/// routes [`crate::config::forces_text_on_compaction`] names; an
/// openai-compat host gets the request as Claude Code sent it.
///
/// A backend that accepts only `auto` ([`ModelFamily::tool_choice_auto_only`])
/// gets any other choice dropped instead of a guaranteed 400, compaction
/// included: Muse wrote the summary as text in every live trial with its
/// tools kept, and keeping them keeps the conversation's cached prefix.
fn rewrite_gpt_body(
    body: &[u8],
    route: &crate::config::ModelRoute,
    compaction: bool,
) -> anyhow::Result<Vec<u8>> {
    let mut document: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| anyhow::anyhow!("invalid JSON: {error}"))?;
    crate::identity::inject_identity_into(
        &mut document,
        &route.display_name,
        route.family.maker(),
    )?;
    if route.family.tool_choice_auto_only() {
        let forced = document.get("tool_choice").is_some_and(|choice| {
            choice.get("type").and_then(serde_json::Value::as_str) != Some("auto")
        });
        if forced && let Some(object) = document.as_object_mut() {
            object.remove("tool_choice");
            tracing::debug!("dropped a tool_choice the backend accepts only as auto");
        }
    } else if compaction
        && crate::config::forces_text_on_compaction(route)
        && document
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
    {
        document["tool_choice"] = serde_json::json!({"type": "none"});
    }
    if route.family.rejects_unanswered_reasoning() {
        let dropped = crate::reasoning_order::drop_unanswered_thinking(&mut document);
        if dropped > 0 {
            tracing::debug!(
                blocks = dropped,
                "dropped thinking that the history answers with no text or tool call"
            );
        }
    }
    if route.family.splits_messages_at_cache_marks() {
        let added = crate::cache_marks::split_at_cache_marks(&mut document);
        if added > 0 {
            tracing::debug!(
                messages = added,
                "ended user messages at their inner cache marks"
            );
        }
    }
    let affected = crate::tool_schema::drop_unportable_patterns(&mut document);
    if !affected.is_empty() {
        tracing::debug!(
            tools = ?affected,
            "dropped tool-schema patterns with Unicode property escapes"
        );
    }
    Ok(serde_json::to_vec(&document)?)
}

async fn gpt_response(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    route: &crate::config::ModelRoute,
    capture: Option<RequestCapture>,
) -> Response {
    // Grok carries reasoning effort in the model ID: the `output_config`
    // field Claude Code sends never reaches xAI (see
    // `routing::effort_qualified_model`).
    let upstream_model = crate::routing::effort_qualified_model(route, &body);
    let rewritten = match substitute_model(&body, &upstream_model) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "failed to rewrite routed model");
            return local_error_response(
                state,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "routed request has an invalid model field",
                capture,
            )
            .await;
        }
    };
    let compaction = parts.headers.contains_key(COMPACTION_HEADER);
    let rewritten = match rewrite_gpt_body(&rewritten, route, compaction) {
        Ok(body) => Bytes::from(body),
        Err(error) => {
            tracing::warn!(%error, "failed to rewrite routed request body");
            return local_error_response(
                state,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "routed request has an unsupported system field shape",
                capture,
            )
            .await;
        }
    };
    let policies = gpt_policies(&rewritten, route);

    // Detected on the inbound body: the rewrite may drop a forced
    // `tool_choice` that detection keys on (see `rewrite_gpt_body`).
    if parts.method == Method::POST
        && parts.uri.path() == "/v1/messages"
        && let Some(subcall) = websearch::detect(&body)
    {
        return search::gpt_branch_subcall(
            state, parts, &body, rewritten, route, &subcall, policies, capture,
        )
        .await;
    }

    let tap = state.websearch_tap(
        &rewritten,
        websearch::Origin::Gpt {
            routing_id: route.routing_id.clone(),
        },
    );
    forward_gpt(
        state,
        parts,
        rewritten,
        &route.upstream_model,
        policies,
        capture,
        tap,
    )
    .await
}

/// The inbound headers a GPT-bound forward should carry: rewritten to the
/// shared-prefix prompt-cache identity of the forwarded body
/// ([`crate::prompt_cache`]), or `None` (use the originals) when no key
/// could be derived. Called by the two GPT egress paths — [`forward_gpt`]
/// and `search::legacy_websearch` — so every GPT-bound request is covered
/// structurally, and Anthropic-bound forwards never are.
fn cache_identity_headers(
    parts: &axum::http::request::Parts,
    forwarded_body: &Bytes,
) -> Option<HeaderMap> {
    let key = crate::prompt_cache::shared_prefix_key(forwarded_body)?;
    Some(headers::with_cache_identity(&parts.headers, &key))
}

/// Everything the GPT branch rewrites in this request's response.
///
/// The input estimate only ever reaches the client through the streamed
/// `message_start`, and counting it is the most expensive thing on this path
/// (a full tiktoken encode of the body, tools included), so a non-streaming
/// request skips it — the buffered web-search paths compute their own — and
/// the overflow translation, armed only for routes with a verified backend
/// dialect and a known real window, defers to the retained body instead
/// (see [`crate::overflow::Estimate`]).
fn gpt_policies(rewritten: &Bytes, route: &crate::config::ModelRoute) -> GptPolicies {
    let streaming = crate::routing::is_streaming(rewritten);
    let estimate = if streaming {
        estimate_input_tokens(rewritten)
    } else {
        0
    };
    let overflow = route
        .context_window
        .zip(crate::config::overflow_dialect(route))
        .map(|(window, dialect)| {
            let overflow_estimate = if streaming {
                crate::overflow::Estimate::Computed(estimate)
            } else {
                crate::overflow::Estimate::Deferred(rewritten.clone())
            };
            OverflowRewrite::new(window, overflow_estimate, dialect)
        });
    GptPolicies {
        estimate,
        scale: route.usage_scale,
        overflow,
    }
}

/// Sends an already-rewritten GPT-branch request to the configured cliproxy
/// upstream (or the stub / an actionable local error).
#[allow(clippy::too_many_arguments)]
async fn forward_gpt(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: Bytes,
    upstream_model: &str,
    policies: GptPolicies,
    capture: Option<RequestCapture>,
    tap: Option<SniffTap>,
) -> Response {
    let cache_headers = cache_identity_headers(parts, &rewritten);
    let inbound_headers = cache_headers.as_ref().unwrap_or(&parts.headers);
    match gpt_target(&state.cliproxy_upstream) {
        GptTarget::Forward(target) => {
            forward(
                state,
                &target.base_url,
                &parts.method,
                &parts.uri,
                inbound_headers,
                rewritten,
                true,
                true,
                target.credential.as_ref(),
                Some(policies),
                capture,
                tap,
            )
            .await
        }
        GptTarget::Stub => local_stub_response(state, upstream_model, &rewritten, capture).await,
        GptTarget::Unavailable(reason) => {
            local_error_response(state, StatusCode::BAD_GATEWAY, "api_error", reason, capture).await
        }
    }
}

/// Where a GPT-branch request can be forwarded.
struct ForwardTarget {
    base_url: String,
    credential: Option<headers::GptUpstreamCredential>,
}

enum GptTarget {
    Forward(ForwardTarget),
    Stub,
    /// No upstream to forward to, with the actionable reason.
    Unavailable(&'static str),
}

/// The one rule for what a GPT-branch request reaches through the cliproxy
/// upstream.
fn gpt_target(upstream: &CliproxyUpstream) -> GptTarget {
    match upstream {
        CliproxyUpstream::Stub => GptTarget::Stub,
        CliproxyUpstream::External {
            base_url,
            credential,
        } => GptTarget::Forward(ForwardTarget {
            base_url: base_url.clone(),
            credential: credential.clone(),
        }),
        CliproxyUpstream::ManagedUnavailable => GptTarget::Unavailable(
            "the cliproxy upstream supervisor failed to start; Claude traffic is unaffected — \
             run `model-router doctor` to diagnose",
        ),
        CliproxyUpstream::Managed(handle) if handle.is_ready() => {
            GptTarget::Forward(ForwardTarget {
                base_url: handle.base_url.clone(),
                credential: Some(handle.credential.clone()),
            })
        }
        CliproxyUpstream::Managed(_) => GptTarget::Unavailable(
            "the cliproxy upstream is not ready (starting up, unauthenticated, or crashed); \
             run `model-router doctor` to diagnose",
        ),
    }
}

/// The upstream GPT-branch side calls are made against, when one exists.
fn gpt_forward_target(upstream: &CliproxyUpstream) -> Option<ForwardTarget> {
    match gpt_target(upstream) {
        GptTarget::Forward(target) => Some(target),
        GptTarget::Stub | GptTarget::Unavailable(_) => None,
    }
}

/// A complete Anthropic message in the framing the client asked for: the SSE
/// event sequence for streaming requests, plain JSON otherwise.
pub(crate) fn frame_message(
    message: &serde_json::Value,
    streaming: bool,
) -> (HeaderMap, Vec<Bytes>) {
    if streaming {
        (sse_headers(), websearch::message_to_sse(message))
    } else {
        (json_headers(), vec![Bytes::from(message.to_string())])
    }
}

/// Headers for a locally produced JSON body.
fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    headers
}

/// Headers for a locally produced SSE stream.
fn sse_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    headers
}

/// Both the response sniffer and the overflow-error translator parse raw
/// response bytes, and the reqwest client does no decompression, so every
/// response the router reads must be identity-encoded. Explicitly `identity`,
/// not merely absent — an absent Accept-Encoding permits the server to pick
/// any coding. Loopback traffic; compression buys nothing here.
fn require_identity_encoding(headers: &mut HeaderMap) {
    headers.insert(
        header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
}

async fn local_stub_response(
    state: &AppState,
    upstream_model: &str,
    request_body: &[u8],
    capture: Option<RequestCapture>,
) -> Response {
    let streaming = crate::routing::is_streaming(request_body);
    let (headers, chunks) = stub::response(upstream_model, streaming);
    local_response(state, StatusCode::OK, headers, chunks, capture).await
}

/// Ingress gate: with a token configured, only `/t/<token>/`-prefixed
/// requests are admitted, with the prefix stripped before routing. The bare
/// health endpoint stays reachable so `doctor` can tell a service that is
/// not running from one that rejects its token.
fn apply_ingress_gate(config: &Config, parts: &mut axum::http::request::Parts) -> bool {
    let Some(token) = &config.ingress_token else {
        return true;
    };
    let prefix = ingress_prefix(token);
    let path = parts.uri.path();
    if parts.method == Method::GET && path == HEALTH_PATH {
        return true;
    }
    let stripped = path.strip_prefix(&prefix).and_then(|rest| {
        if rest.is_empty() {
            Some("/".to_string())
        } else if rest.starts_with('/') {
            Some(rest.to_string())
        } else {
            None
        }
    });
    let Some(stripped) = stripped else {
        return false;
    };
    let rewritten = match parts.uri.query() {
        Some(query) => format!("{stripped}?{query}"),
        None => stripped,
    };
    let Ok(uri) = rewritten.parse::<Uri>() else {
        return false;
    };
    parts.uri = uri;
    true
}

/// Local liveness/version endpoint for the `SessionStart` hook and doctor.
fn health_response(state: &AppState) -> Response {
    let (upstream, healthy) = match &state.cliproxy_upstream {
        CliproxyUpstream::Stub => ("stub", true),
        CliproxyUpstream::External { .. } => ("external", true),
        CliproxyUpstream::ManagedUnavailable => ("unavailable", false),
        CliproxyUpstream::Managed(handle) if handle.is_ready() => ("ready", true),
        CliproxyUpstream::Managed(_) => ("not-ready", false),
    };
    let bytes = Bytes::from(
        json!({
            "status": if healthy { "ok" } else { "degraded" },
            "version": env!("CARGO_PKG_VERSION"),
            "cliproxy-upstream": upstream,
            // What this process resolved at startup (null: undeclared).
            // Doctor compares it with a fresh read so a settings change the
            // running service has not picked up is visible rather than
            // silently mis-scaling.
            "declared-context-window": state.config.declared_context_window,
        })
        .to_string(),
    );
    build_response(StatusCode::OK, json_headers(), Body::from(bytes))
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: &AppState,
    upstream_base: &str,
    method: &Method,
    uri: &Uri,
    inbound_headers: &HeaderMap,
    body: Bytes,
    strip_credentials: bool,
    body_changed: bool,
    gpt_credential: Option<&headers::GptUpstreamCredential>,
    policies: Option<GptPolicies>,
    mut capture: Option<RequestCapture>,
    tap: Option<SniffTap>,
) -> Response {
    let url = upstream_url(upstream_base, uri);
    let mut outgoing_headers = headers::request_headers(
        inbound_headers,
        strip_credentials,
        body_changed,
        gpt_credential,
    );
    if tap.is_some() || policies.is_some() {
        require_identity_encoding(&mut outgoing_headers);
    }
    // A capture records what left the router.
    if let Some(request_capture) = &mut capture {
        request_capture.headers = redact_headers(&outgoing_headers);
    }
    match state
        .client
        .request(method.clone(), url)
        .headers(outgoing_headers)
        .body(body)
        .send()
        .await
    {
        Ok(response) => upstream_response(state, response, capture, policies, tap).await,
        Err(error) => {
            tracing::warn!(%error, "upstream request failed");
            local_error_response(
                state,
                StatusCode::BAD_GATEWAY,
                "api_error",
                "upstream request failed",
                capture,
            )
            .await
        }
    }
}

async fn upstream_response(
    state: &AppState,
    response: reqwest::Response,
    capture: Option<RequestCapture>,
    policies: Option<GptPolicies>,
    tap: Option<SniffTap>,
) -> Response {
    let status = response.status();
    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("text/event-stream")
            })
        });
    if status == StatusCode::BAD_REQUEST
        && !is_sse
        && let Some(overflow) = policies
            .as_ref()
            .and_then(|policies| policies.overflow.as_ref())
    {
        return overflow_translated_response(state, status, response, overflow, capture).await;
    }
    let transform_usage = policies.is_some() && is_sse;
    let response_headers = headers::response_headers(response.headers(), transform_usage);
    let stream = response.bytes_stream();
    let stream: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>> =
        if let Some(tap) = tap.filter(|_| status == StatusCode::OK && is_sse) {
            // Passive tee: parse each chunk and commit any completed
            // WebSearch observations BEFORE yielding it, so the client can
            // never issue the follow-up sub-call ahead of the pending entry.
            // Bytes are forwarded unchanged whether or not parsing succeeds.
            let mut sniffer = websearch::ToolUseSniffer::new();
            Box::pin(async_stream::stream! {
                let mut inner = Box::pin(stream);
                while let Some(item) = inner.next().await {
                    if let Ok(bytes) = &item {
                        for search in sniffer.push(bytes) {
                            tap.pending.insert(
                                websearch::PendingKey::for_sniffed(tap.session_id.clone(), &search),
                                tap.origin.clone(),
                            );
                        }
                    }
                    yield item;
                }
            })
        } else {
            Box::pin(stream)
        };

    let stream: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>> =
        if let Some(policies) = policies.filter(|_| transform_usage) {
            Box::pin(async_stream::stream! {
            let mut stream = stream;
            let mut transformer = SseUsageTransformer::new(policies);
            while let Some(item) = stream.next().await {
                match item {
                    Ok(bytes) => {
                        for transformed in transformer.push(&bytes) {
                            yield Ok::<Bytes, reqwest::Error>(transformed);
                        }
                    }
                    Err(error) => {
                        if let Some(buffered) = transformer.finish() {
                            yield Ok(buffered);
                        }
                        yield Err(error);
                        return;
                    }
                }
            }
            if let Some(buffered) = transformer.finish() {
                yield Ok(buffered);
            }
            })
        } else {
            stream
        };
    let body = streaming_response_body(state, status, &response_headers, capture, stream);
    build_response(status, response_headers, body)
}

/// Upper bound on a buffered GPT error body. The captured overflow error is
/// ~160 bytes; anything past this bound is not the error we are looking for
/// and streams through untouched.
const OVERFLOW_BUFFER_LIMIT: usize = 64 * 1024;

/// A non-streaming 400 on a route with overflow translation armed: buffer
/// the (small) body, rewrite it when it is the Codex context-overflow error
/// (see [`crate::overflow`]), and pass everything else through unchanged.
async fn overflow_translated_response(
    state: &AppState,
    status: StatusCode,
    response: reqwest::Response,
    overflow: &OverflowRewrite,
    capture: Option<RequestCapture>,
) -> Response {
    let passthrough_headers = headers::response_headers(response.headers(), false);
    let rewritten_headers = headers::response_headers(response.headers(), true);
    let mut stream = response.bytes_stream();
    let mut chunks: Vec<Bytes> = Vec::new();
    let mut total = 0usize;
    loop {
        match stream.next().await {
            Some(Ok(bytes)) => {
                total = total.saturating_add(bytes.len());
                chunks.push(bytes);
                if total > OVERFLOW_BUFFER_LIMIT {
                    let rest = futures_util::stream::iter(
                        chunks.into_iter().map(Ok::<Bytes, reqwest::Error>),
                    )
                    .chain(stream);
                    let body =
                        streaming_response_body(state, status, &passthrough_headers, capture, rest);
                    return build_response(status, passthrough_headers, body);
                }
            }
            Some(Err(error)) => {
                // Forward the prefix and the failure exactly as pass-through
                // streaming would have.
                let rest =
                    futures_util::stream::iter(chunks.into_iter().map(Ok).chain([Err(error)]));
                let body =
                    streaming_response_body(state, status, &passthrough_headers, capture, rest);
                return build_response(status, passthrough_headers, body);
            }
            None => break,
        }
    }
    let body = chunks.concat();
    if let Some(rewritten) = overflow.rewrite_body(&body) {
        tracing::info!("translated a GPT context-overflow error to the canonical Anthropic form");
        // The body changed length; rewritten_headers dropped the upstream's
        // Content-Length so the axum layer restates it.
        return local_response(state, status, rewritten_headers, vec![rewritten], capture).await;
    }
    local_response(
        state,
        status,
        passthrough_headers,
        vec![Bytes::from(body)],
        capture,
    )
    .await
}

fn streaming_response_body<S>(
    state: &AppState,
    status: StatusCode,
    response_headers: &HeaderMap,
    capture: Option<RequestCapture>,
    stream: S,
) -> Body
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    if let (Some(sink), Some(request_capture)) = (state.capture.clone(), capture) {
        let mut capture = StreamingCapture::new(
            sink,
            request_capture,
            status.as_u16(),
            response_headers.clone(),
        );
        let capture_stream = async_stream::stream! {
            let mut stream = Box::pin(stream);
            while let Some(item) = stream.next().await {
                match item {
                    Ok(bytes) => {
                        capture.push(&bytes);
                        yield Ok::<Bytes, reqwest::Error>(bytes);
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
            if let Err(error) = capture.finish().await {
                tracing::error!(%error, "failed to append capture record");
            }
        };
        Body::from_stream(capture_stream)
    } else {
        Body::from_stream(stream)
    }
}

async fn local_error_response(
    state: &AppState,
    status: StatusCode,
    error_type: &str,
    message: &str,
    capture: Option<RequestCapture>,
) -> Response {
    let bytes = Bytes::from(
        json!({"type":"error","error":{"type":error_type,"message":message}}).to_string(),
    );
    local_response(state, status, json_headers(), vec![bytes], capture).await
}

async fn local_response(
    state: &AppState,
    status: StatusCode,
    response_headers: HeaderMap,
    chunks: Vec<Bytes>,
    capture: Option<RequestCapture>,
) -> Response {
    if let (Some(sink), Some(request_capture)) = (&state.capture, capture) {
        let mut body = sink.response_body_capture();
        for chunk in &chunks {
            body.push(chunk);
        }
        if let Err(error) = sink
            .append_captured(request_capture, status.as_u16(), &response_headers, body)
            .await
        {
            tracing::error!(%error, "failed to append capture record");
        }
    }
    let stream = futures_util::stream::iter(chunks.into_iter().map(Ok::<Bytes, Infallible>));
    build_response(status, response_headers, Body::from_stream(stream))
}

fn build_response(status: StatusCode, headers: HeaderMap, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn upstream_url(base: &str, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map_or_else(|| uri.path(), axum::http::uri::PathAndQuery::as_str);
    format!("{}{path_and_query}", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CaptureConfig, ModelRoute, UpstreamConfig, UpstreamMode};
    use tower::ServiceExt;

    async fn held_stream(State(started): State<Arc<tokio::sync::Notify>>) -> Body {
        started.notify_one();
        Body::from_stream(futures_util::stream::pending::<Result<Bytes, Infallible>>())
    }

    #[tokio::test]
    async fn managed_mode_without_supervisor_serves_degraded() {
        // Managed mode with no supervisor handle must still build the app
        // (Claude traffic keeps flowing); health reports the degraded state.
        let app = app(Config::default()).await.unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/__model-router/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "degraded");
        assert_eq!(health["cliproxy-upstream"], "unavailable");
        // Present even when undeclared, so doctor can tell an undeclared
        // service from one too old to report it.
        assert_eq!(
            health.get("declared-context-window"),
            Some(&serde_json::Value::Null)
        );
    }

    #[tokio::test]
    async fn shutdown_drain_drops_a_held_connection_at_the_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route("/held", axum::routing::get(held_stream))
            .with_state(started.clone());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve_app(
            listener,
            app,
            Arc::new(SearchStreams::new()),
            async move {
                let _ = shutdown_rx.await;
            },
            std::time::Duration::from_millis(50),
        ));
        let request = tokio::spawn(reqwest::get(format!("http://{address}/held")));
        started.notified().await;
        let response = request.await.unwrap().unwrap();

        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(500), server)
            .await
            .expect("held connection blocked serve past the drain bound")
            .unwrap()
            .unwrap();
        drop(response);
    }

    #[tokio::test]
    async fn capture_truncates_at_limit_without_truncating_client_stream() {
        const CAPTURE_LIMIT: usize = 64;
        let directory = tempfile::tempdir().unwrap();
        let capture_file = directory.path().join("capture.jsonl");
        let config = Config {
            upstreams: std::collections::BTreeMap::from([(
                "cliproxy".to_string(),
                UpstreamConfig {
                    mode: UpstreamMode::Stub,
                    ..UpstreamConfig::default()
                },
            )]),
            models: vec![ModelRoute {
                routing_id: "claude-gpt-test".to_string(),
                upstream: "cliproxy".to_string(),
                upstream_model: "gpt-test".to_string(),
                display_name: "GPT Test".to_string(),
                ..Default::default()
            }],
            capture: CaptureConfig {
                enabled: true,
                file: capture_file.clone(),
                max_response_body_bytes: CAPTURE_LIMIT,
            },
            ..Config::default()
        };
        let app = app(config).await.unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"claude-gpt-test","stream":true,"messages":[]}"#,
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let client_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(client_body.len() > CAPTURE_LIMIT);
        assert!(String::from_utf8_lossy(&client_body).contains("event: message_stop"));

        let jsonl = tokio::fs::read_to_string(capture_file).await.unwrap();
        let record: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        assert_eq!(record["response_body_truncated"], true);
        assert_eq!(record["response_body_captured_bytes"], CAPTURE_LIMIT);
        assert_eq!(record["response_body_received_bytes"], client_body.len());
        assert_eq!(
            record["response_body"].as_str().unwrap().len(),
            CAPTURE_LIMIT
        );
    }
}
