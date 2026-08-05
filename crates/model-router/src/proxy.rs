use std::collections::HashSet;
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
use crate::config::{Config, UpstreamMode, WebSearchMode};
use crate::headers;
use crate::overflow::OverflowRewrite;
use crate::routing::{Branch, RoutingDecision, decide, substitute_model};
use crate::stub;
use crate::usage::{GptPolicies, SseUsageTransformer, UsagePolicy, estimate_input_tokens};
use crate::websearch;

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
    /// In-flight xAI search streams, owned so shutdown can wait for them.
    xai_searches: Arc<XaiSearchTasks>,
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
                    .transpose()?,
            }),
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
        let xai_searches = Arc::new(XaiSearchTasks::new(config.xai_search));
        Ok(Self {
            config: Arc::new(config),
            client,
            capture,
            cliproxy_upstream,
            pending_searches: Arc::new(websearch::PendingSearches::default()),
            xai_searches,
        })
    }

    /// Builds the response tap for a `/v1/messages` request when the
    /// web-search feature is on and the request declares the client
    /// `WebSearch` tool (cheap byte prefilter; a false positive merely arms
    /// the passive tap).
    fn websearch_tap(&self, body: &[u8], origin: websearch::Origin) -> Option<SniffTap> {
        if self.config.web_search.mode == WebSearchMode::Off
            || !websearch::declares_websearch_tool(body)
        {
            return None;
        }
        Some(SniffTap {
            pending: self.pending_searches.clone(),
            session_id: websearch::session_id(body)?,
            origin,
        })
    }
}

/// The unauthenticated introspection endpoint: exempt from the ingress gate
/// and matched by the handler. One constant so the two can never drift.
pub const HEALTH_PATH: &str = "/__model-router/health";

/// The tokened path prefix the ingress gate accepts. Doctor's `base_url` and
/// the startup log must build URLs through this same function.
#[must_use]
pub fn ingress_prefix(token: &str) -> String {
    format!("/t/{token}")
}

/// The full gateway base URL for a given bind address and ingress token —
/// the value Claude Code uses as `ANTHROPIC_BASE_URL`.
#[must_use]
pub fn tokened_base_url(address: &SocketAddr, token: &str) -> String {
    format!("http://{address}{}", ingress_prefix(token))
}

/// Serves using an already-bound loopback listener (primarily for tests).
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
    searches: Arc<XaiSearchTasks>,
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
            // drain left, and before the caller tears the managed child down.
            // Dropping one would disconnect it mid-response and quarantine
            // the xAI auth.
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
    app_with(config, None).await
}

/// Builds the gateway service with an optional managed-upstream handle.
///
/// # Errors
/// Returns an error for invalid configuration, capture-file failures, or HTTP
/// client initialization failures.
pub async fn app_with(
    config: Config,
    managed: Option<crate::supervisor::ManagedHandle>,
) -> anyhow::Result<Router> {
    Ok(build(config, managed).await?.0)
}

/// The router plus the handle shutdown needs to wait for in-flight xAI search
/// streams.
async fn build(
    config: Config,
    managed: Option<crate::supervisor::ManagedHandle>,
) -> anyhow::Result<(Router, Arc<XaiSearchTasks>)> {
    let state = AppState::new(config, managed).await?;
    let searches = Arc::clone(&state.xai_searches);
    Ok((
        Router::new().fallback(any(handle)).with_state(state),
        searches,
    ))
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();

    if let Err(response) = apply_ingress_gate(&state.config, &mut parts) {
        return response;
    }

    let body = match to_bytes(body, state.config.max_request_body_bytes).await {
        Ok(body) => body,
        Err(error) => {
            let over_limit = std::error::Error::source(&error)
                .is_some_and(<dyn std::error::Error + 'static>::is::<axum::http::Error>)
                || error.to_string().contains("length limit exceeded");
            tracing::warn!(%error, over_limit, "failed to read inbound request body");
            if over_limit {
                return error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request_error",
                    "request body exceeds max-request-body-bytes",
                );
            }
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "failed to read request body",
            );
        }
    };

    if parts.method == Method::GET && parts.uri.path() == HEALTH_PATH {
        return health_response(&state);
    }

    let decision = decide(&state.config, &body);
    let capture = state.capture.as_ref().map(|_| RequestCapture {
        branch: decision.branch.as_str().to_string(),
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
        branch = decision.branch.as_str(),
        family = decision.family_label(),
        model = decision.model.as_deref().unwrap_or("<none>"),
        "routing request"
    );

    if parts.method == Method::GET && parts.uri.path() == "/v1/models" {
        return models_response(
            &state,
            &parts.headers,
            &parts.method,
            &parts.uri,
            body,
            capture,
        )
        .await;
    }

    if parts.method == Method::POST
        && parts.uri.path() == "/v1/messages/count_tokens"
        && decision.branch == Branch::Gpt
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

    match decision.branch {
        Branch::Claude => claude_response(&state, &parts, body, &decision, capture).await,
        Branch::Gpt => gpt_response(&state, &parts, body, &decision, capture).await,
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
    if state.config.web_search.mode != WebSearchMode::Off && is_messages_post {
        if let Some(subcall) = websearch::detect(&body) {
            let origin = websearch::session_id(&body).and_then(|session_id| {
                state.pending_searches.consume(&websearch::PendingKey::new(
                    session_id,
                    &subcall.query,
                    subcall.allowed_domains.as_deref(),
                    subcall.blocked_domains.as_deref(),
                ))
            });
            if let Some(websearch::Origin::Gpt { routing_id }) = origin {
                return gpt_origin_on_claude_branch(
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

/// Answers a sub-call on the origin's own route by forwarding it to that
/// route's upstream and scraping links out of the response text.
///
/// Shared by the `scrape` mode arm and by `alpha` mode's failure path, so
/// the two degrade identically — the sub-call stays with the vendor the
/// user picked for the work. `Err` returns the capture so the caller can
/// continue to the Anthropic passthrough.
#[allow(clippy::too_many_arguments)] // mirrors websearch_response's shape
async fn scrape_on_origin_route(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: &Bytes,
    route: &crate::config::ModelRoute,
    subcall: &websearch::Subcall,
    base_url: &str,
    credential: Option<&headers::GptUpstreamCredential>,
    capture: Option<RequestCapture>,
) -> Result<Response, Option<RequestCapture>> {
    let rewritten = match substitute_model(body, &route.upstream_model) {
        Ok(rewritten) => Bytes::from(rewritten),
        Err(error) => {
            tracing::warn!(%error, "failed to rewrite sub-call model; passing through to Anthropic");
            return Err(capture);
        }
    };
    match legacy_websearch(state, parts, &rewritten, base_url, credential).await {
        Ok(mut message) => {
            let filled = websearch::fill_empty_web_search_results(&mut message);
            tracing::info!(
                filled,
                origin = %route.routing_id,
                "scraped links into the routed-origin web search response"
            );
            Ok(message_response(state, &message, subcall.stream, capture).await)
        }
        Err(error) => {
            tracing::warn!(%error, "routed web search forward failed; passing the sub-call through to Anthropic");
            Err(capture)
        }
    }
}

/// A sub-call arriving on the Claude branch whose `WebSearch` was invoked by
/// a routed agent (GPT, Grok, or open-weights): answer from the matching
/// backend. `alpha` mode asks the Codex search backend and, on failure,
/// falls back to the origin route's own scrape path — never straight to
/// Anthropic, which would silently move the work and the bill to a vendor
/// the user did not choose. `scrape` mode goes to that same path directly.
/// The native Anthropic passthrough is the last resort only.
async fn gpt_origin_on_claude_branch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    routing_id: &str,
    subcall: &websearch::Subcall,
    capture: Option<RequestCapture>,
) -> Response {
    let mut capture = capture;
    let route = state
        .config
        .effective_models()
        .find(|route| route.routing_id == routing_id);
    // A Grok origin is served by xAI's own search, strictly: no alpha, no
    // scrape, no Anthropic — not even when the gateway is unavailable. An
    // origin whose route has since left the config has no known family, so it
    // keeps the existing behaviour below.
    if let Some(route) = route
        && websearch::uses_native_search(route.family)
    {
        return grok_native_websearch_response(state, &body, route, subcall, capture).await;
    }
    let target = gpt_forward_target(&state.cliproxy_upstream);
    if let (Some(route), Some((base_url, credential))) = (route, target) {
        let mode = state.config.web_search.mode;
        if mode == WebSearchMode::Alpha {
            match alpha_search(
                state,
                &base_url,
                credential.as_ref(),
                &route.upstream_model,
                subcall,
            )
            .await
            {
                Ok((links, output)) => {
                    tracing::info!(
                        links = links.len(),
                        origin = routing_id,
                        backend = "alpha-search",
                        "answered routed-origin web search from alpha/search"
                    );
                    let message = websearch::synthesize_message(
                        &route.routing_id,
                        subcall,
                        &links,
                        &output,
                        estimate_input_tokens(&body),
                    );
                    return message_response(state, &message, subcall.stream, capture).await;
                }
                Err(error) => {
                    tracing::warn!(%error, "alpha web search failed; falling back to the origin route's scrape path");
                }
            }
        }
        if matches!(mode, WebSearchMode::Alpha | WebSearchMode::Scrape) {
            match scrape_on_origin_route(
                state,
                parts,
                &body,
                route,
                subcall,
                &base_url,
                credential.as_ref(),
                capture,
            )
            .await
            {
                Ok(response) => return response,
                Err(returned) => capture = returned,
            }
        }
    }
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
        None,
    )
    .await
}

async fn gpt_response(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    decision: &RoutingDecision<'_>,
    capture: Option<RequestCapture>,
) -> Response {
    let route = decision
        .route
        .expect("GPT decisions always contain an allowlist route");
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
    let rewritten = match crate::identity::inject_identity(&rewritten, &route.display_name) {
        Ok(body) => Bytes::from(body),
        Err(error) => {
            tracing::warn!(%error, "failed to inject identity block");
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

    if state.config.web_search.mode != WebSearchMode::Off
        && parts.method == Method::POST
        && parts.uri.path() == "/v1/messages"
        && let Some(subcall) = websearch::detect(&rewritten)
    {
        return gpt_branch_subcall(
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
/// and [`legacy_websearch`] — so every GPT-bound request is covered
/// structurally, and Anthropic-bound forwards never are.
fn cache_identity_headers(
    parts: &axum::http::request::Parts,
    forwarded_body: &Bytes,
) -> Option<HeaderMap> {
    let key = crate::prompt_cache::shared_prefix_key(forwarded_body)?;
    headers::with_cache_identity(&parts.headers, &key)
}

/// A `WebSearch` sub-call that arrived on the GPT branch: answered by the
/// origin's own backend, else by the existing alpha/scrape/forward ladder.
#[allow(clippy::too_many_arguments)] // mirrors websearch_response's shape
async fn gpt_branch_subcall(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: &Bytes,
    rewritten: Bytes,
    route: &crate::config::ModelRoute,
    subcall: &websearch::Subcall,
    policies: GptPolicies,
    capture: Option<RequestCapture>,
) -> Response {
    let origin = websearch::session_id(&rewritten).and_then(|session_id| {
        state.pending_searches.consume(&websearch::PendingKey::new(
            session_id,
            &subcall.query,
            subcall.allowed_domains.as_deref(),
            subcall.blocked_domains.as_deref(),
        ))
    });
    let mut capture = capture;
    // Policy lives in `websearch::native_search_route` (unit-tested there);
    // this only supplies the config lookup. `None` leaves every arm below
    // exactly as it was, still using `route`.
    let native_search_route =
        websearch::native_search_route(origin.as_ref(), route, |routing_id| {
            state
                .config
                .effective_models()
                .find(|candidate| candidate.routing_id == routing_id)
        });
    // The Anthropic-native path needs no GPT upstream — attempt it even when
    // the GPT target is unavailable.
    if let Some(websearch::Origin::Claude { model }) = origin {
        match claude_origin_on_gpt_branch(state, parts, body, &model, subcall, capture).await {
            Ok(response) => return response,
            Err(returned) => capture = returned,
        }
    }
    if let Some(grok) = native_search_route {
        return grok_native_websearch_response(state, &rewritten, grok, subcall, capture).await;
    }
    if let Some((base_url, credential)) = gpt_forward_target(&state.cliproxy_upstream) {
        return websearch_response(
            state,
            parts,
            rewritten,
            &route.routing_id,
            &route.upstream_model,
            subcall,
            &base_url,
            credential.as_ref(),
            policies,
            capture,
        )
        .await;
    }
    // No GPT target (stub / unready managed): existing handling.
    forward_gpt(
        state,
        parts,
        rewritten,
        &route.upstream_model,
        policies,
        capture,
        None,
    )
    .await
}

/// How this request's usage is reported back.
///
/// The estimate only ever reaches the client through the streamed
/// `message_start`, and counting it is the most expensive thing on this path
/// (a full tiktoken encode of the body, tools included), so a non-streaming
/// request skips it — the buffered web-search paths compute their own.
fn usage_policy(rewritten: &Bytes, route: &crate::config::ModelRoute) -> UsagePolicy {
    UsagePolicy {
        estimate: if crate::routing::is_streaming(rewritten) {
            estimate_input_tokens(rewritten)
        } else {
            0
        },
        scale: route.usage_scale,
    }
}

/// Everything the GPT branch rewrites in this request's response. Overflow
/// translation is armed only for routes with a verified backend dialect and
/// a known real window, and only ever matches that dialect's own phrase. A streaming request carries its already-computed estimate;
/// a non-streaming one carries the body (a refcount, not a copy) for lazy
/// estimation — never both, so a long-lived response stream does not pin
/// its request's payload.
fn gpt_policies(rewritten: &Bytes, route: &crate::config::ModelRoute) -> GptPolicies {
    let usage = usage_policy(rewritten, route);
    let overflow = route
        .context_window
        .zip(crate::config::overflow_dialect(route))
        .map(|(window, dialect)| {
            let estimate = if crate::routing::is_streaming(rewritten) {
                crate::overflow::Estimate::Computed(usage.estimate)
            } else {
                crate::overflow::Estimate::Deferred(rewritten.clone())
            };
            OverflowRewrite::new(window, estimate, dialect)
        });
    GptPolicies { usage, overflow }
}

/// A sub-call arriving on the GPT branch whose `WebSearch` was invoked by a
/// Claude agent: answer it from Anthropic natively. `Err` returns the
/// capture record so the caller can continue with the GPT path (transport /
/// transient failures only; caller-error statuses are surfaced).
async fn claude_origin_on_gpt_branch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: &Bytes,
    origin_model: &str,
    subcall: &websearch::Subcall,
    capture: Option<RequestCapture>,
) -> Result<Response, Option<RequestCapture>> {
    match anthropic_native_websearch(state, parts, body, origin_model).await {
        NativeOutcome::Message(message) => {
            tracing::info!(origin = %origin_model, "answered Claude-origin web search from Anthropic");
            Ok(message_response(state, &message, subcall.stream, capture).await)
        }
        NativeOutcome::Surface(status, response_headers, body) => {
            Ok(local_response(state, status, response_headers, vec![body], capture).await)
        }
        NativeOutcome::Fallback(error) => {
            tracing::warn!(%error, "Anthropic-native web search failed; using the GPT path");
            Err(capture)
        }
    }
}

/// What came of forwarding a Claude-origin sub-call to Anthropic natively.
enum NativeOutcome {
    /// A complete Anthropic message, ready for re-framing.
    Message(serde_json::Value),
    /// A non-transient Anthropic response that must reach the client
    /// unchanged — silently switching providers would mask config/auth/
    /// request defects.
    Surface(StatusCode, HeaderMap, Bytes),
    /// Transport failure or transient upstream error (429/5xx): eligible for
    /// the GPT fallback (nothing has been streamed to the client yet).
    Fallback(anyhow::Error),
}

/// Forwards a sub-call to Anthropic on behalf of a Claude-origin agent, as a
/// buffered non-streaming request built for the origin model (see
/// [`websearch::native_request_body`]). Inbound Anthropic credentials and
/// `anthropic-beta` are preserved (Claude-branch header semantics).
async fn anthropic_native_websearch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    original_body: &Bytes,
    origin_model: &str,
) -> NativeOutcome {
    let Some(body) = websearch::native_request_body(original_body, origin_model) else {
        return NativeOutcome::Fallback(anyhow::anyhow!(
            "sub-call body could not be normalized for the origin model"
        ));
    };
    let mut outgoing_headers = headers::request_headers(&parts.headers, false, true, None);
    // The body is parsed here, and the reqwest client does no decompression;
    // explicitly `identity` — an absent Accept-Encoding permits any coding.
    outgoing_headers.insert(
        header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
    let result = state
        .client
        .request(
            parts.method.clone(),
            upstream_url(&state.config.anthropic_upstream_base, &parts.uri),
        )
        .headers(outgoing_headers)
        .body(body)
        .timeout(std::time::Duration::from_mins(4))
        .send()
        .await;
    let response = match result {
        Ok(response) => response,
        Err(error) => return NativeOutcome::Fallback(error.into()),
    };
    let status = StatusCode::from_u16(response.status().as_u16()).expect("valid HTTP status");
    let response_headers = headers::response_headers(response.headers(), true);
    let fallback_eligible = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => return NativeOutcome::Fallback(error.into()),
    };
    if status != StatusCode::OK {
        if fallback_eligible {
            return NativeOutcome::Fallback(anyhow::anyhow!(
                "Anthropic returned HTTP {}",
                status.as_u16()
            ));
        }
        // Everything else — 4xx, redirects, unexpected statuses — reflects
        // the request or configuration, not transience: surface it.
        return NativeOutcome::Surface(status, response_headers, bytes);
    }
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(message)
            if message.get("type").and_then(serde_json::Value::as_str) == Some("message") =>
        {
            NativeOutcome::Message(message)
        }
        // A complete 200 with a body that is not an Anthropic message is a
        // protocol/configuration defect (e.g. an intermediary's HTML error
        // page), not transience: surface it rather than switch providers.
        Ok(_) | Err(_) => {
            tracing::warn!("Anthropic returned HTTP 200 with a non-message body; surfacing it");
            NativeOutcome::Surface(status, response_headers, bytes)
        }
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
    match &state.cliproxy_upstream {
        CliproxyUpstream::Stub => {
            local_stub_response(state, upstream_model, &rewritten, capture).await
        }
        CliproxyUpstream::External {
            base_url,
            credential,
        } => {
            forward(
                state,
                base_url,
                &parts.method,
                &parts.uri,
                inbound_headers,
                rewritten,
                true,
                true,
                credential.as_ref(),
                Some(policies),
                capture,
                tap,
            )
            .await
        }
        CliproxyUpstream::ManagedUnavailable => {
            local_error_response(
                state,
                StatusCode::BAD_GATEWAY,
                "api_error",
                "the cliproxy upstream supervisor failed to start; Claude traffic is unaffected — \
                 run `model-router doctor` to diagnose",
                capture,
            )
            .await
        }
        CliproxyUpstream::Managed(handle) => {
            if !handle.is_ready() {
                return local_error_response(
                    state,
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    "the cliproxy upstream is not ready (starting up, unauthenticated, or crashed); \
                     run `model-router doctor` to diagnose",
                    capture,
                )
                .await;
            }
            forward(
                state,
                &handle.base_url,
                &parts.method,
                &parts.uri,
                inbound_headers,
                rewritten,
                true,
                true,
                Some(&handle.credential),
                Some(policies),
                capture,
                tap,
            )
            .await
        }
    }
}

/// The (base URL, credential) pair GPT-branch requests are forwarded to, when
/// one exists. Stub and not-yet-ready managed upstreams return `None` and
/// keep their existing handling.
fn gpt_forward_target(
    upstream: &CliproxyUpstream,
) -> Option<(String, Option<headers::GptUpstreamCredential>)> {
    match upstream {
        CliproxyUpstream::External {
            base_url,
            credential,
        } => Some((base_url.clone(), credential.clone())),
        CliproxyUpstream::Managed(handle) if handle.is_ready() => {
            Some((handle.base_url.clone(), Some(handle.credential.clone())))
        }
        CliproxyUpstream::Stub
        | CliproxyUpstream::Managed(_)
        | CliproxyUpstream::ManagedUnavailable => None,
    }
}

/// Answers a detected `WebSearch` sub-call: from the Codex search backend in
/// `alpha` mode, else (or on failure) via the LLM upstream with links scraped
/// into the empty result blocks, else plain forwarding as the last resort.
#[allow(clippy::too_many_arguments)]
async fn websearch_response(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: Bytes,
    routing_id: &str,
    upstream_model: &str,
    subcall: &websearch::Subcall,
    base_url: &str,
    credential: Option<&headers::GptUpstreamCredential>,
    policies: GptPolicies,
    capture: Option<RequestCapture>,
) -> Response {
    if state.config.web_search.mode == WebSearchMode::Alpha {
        match alpha_search(state, base_url, credential, upstream_model, subcall).await {
            Ok((links, output)) => {
                tracing::info!(
                    links = links.len(),
                    backend = "alpha-search",
                    "answered web search from alpha/search"
                );
                let message = websearch::synthesize_message(
                    routing_id,
                    subcall,
                    &links,
                    &output,
                    // Sub-call responses are not conversation context, so they
                    // are reported unscaled.
                    estimate_input_tokens(&rewritten),
                );
                return message_response(state, &message, subcall.stream, capture).await;
            }
            Err(error) => {
                tracing::warn!(%error, "alpha web search failed; falling back to the LLM web search path");
            }
        }
    }
    match legacy_websearch(state, parts, &rewritten, base_url, credential).await {
        Ok(mut message) => {
            if let Some(usage) = message.get_mut("usage") {
                // Sub-call responses are conversation context for nobody, so
                // they get the estimate but never the route's scale.
                crate::usage::inject_estimated_input_tokens(
                    usage,
                    estimate_input_tokens(&rewritten),
                );
            }
            let filled = websearch::fill_empty_web_search_results(&mut message);
            tracing::info!(filled, "scraped links into the LLM web search response");
            message_response(state, &message, subcall.stream, capture).await
        }
        Err(error) => {
            tracing::warn!(%error, "buffered web search forward failed; passing the sub-call through");
            forward_gpt(
                state,
                parts,
                rewritten,
                upstream_model,
                policies,
                capture,
                None,
            )
            .await
        }
    }
}

/// Why an xAI-native search could not be answered. Carries its own mapping to
/// the client-visible failed-search result, so the taxonomy lives in one place.
enum SearchFailure {
    Status(u16),
    Transport(anyhow::Error),
    TimedOut,
    NoSources(&'static str),
    GatewayUnavailable,
    /// The gateway is shutting down and will not start new searches.
    ShuttingDown,
    /// The sub-call's domain filter cannot be honored.
    InvalidFilter(websearch::InvalidDomainRule),
}

impl SearchFailure {
    /// `(error_code, detail)` for [`websearch::synthesize_error_message`].
    fn rendered(&self) -> (&'static str, String) {
        match self {
            Self::Status(429) => (
                websearch::search_error::TOO_MANY_REQUESTS,
                "xAI search is rate-limited".to_string(),
            ),
            Self::Status(status) => (
                websearch::search_error::UNAVAILABLE,
                format!("xAI search returned HTTP {status}"),
            ),
            Self::Transport(error) => (
                websearch::search_error::UNAVAILABLE,
                format!("xAI search could not be reached ({error})"),
            ),
            Self::TimedOut => (
                websearch::search_error::UNAVAILABLE,
                "xAI search did not return sources in time".to_string(),
            ),
            Self::NoSources(reason) => (
                websearch::search_error::UNAVAILABLE,
                format!("xAI search returned no sources ({reason})"),
            ),
            Self::GatewayUnavailable => (
                websearch::search_error::UNAVAILABLE,
                "the model gateway is not ready".to_string(),
            ),
            Self::ShuttingDown => (
                websearch::search_error::UNAVAILABLE,
                "the model gateway is shutting down".to_string(),
            ),
            Self::InvalidFilter(rule) => (
                websearch::search_error::INVALID_INPUT,
                // Refusing beats quietly widening an allow-list or dropping a
                // block-list the caller asked for.
                rule.to_string(),
            ),
        }
    }
}

/// Owns every in-flight xAI search stream.
///
/// Each search runs in a task that reads its response to completion no matter
/// what the request handler does, because dropping a live stream disconnects
/// the client mid-response and makes the child quarantine the xAI auth entry
/// for 30–60s (measured). The handler's deadline therefore stops it WAITING;
/// it never cancels the HTTP read. Shutdown stops admitting new searches and
/// waits for the running ones, so a restart cannot orphan a stream either.
///
/// How many searches may run at once is not the router's business: a parallel
/// sweep should meet the limits the user's own xAI subscription imposes, not
/// an invented one. Admission is a shutdown gate, nothing more.
pub struct XaiSearchTasks {
    limits: crate::config::XaiSearchLimits,
    closed: std::sync::atomic::AtomicBool,
    tasks: tokio::sync::Mutex<tokio::task::JoinSet<()>>,
}

impl XaiSearchTasks {
    fn new(limits: crate::config::XaiSearchLimits) -> Self {
        Self {
            closed: std::sync::atomic::AtomicBool::new(false),
            tasks: tokio::sync::Mutex::new(tokio::task::JoinSet::new()),
            limits,
        }
    }

    /// Takes ownership of one search stream's task. `false` once shutdown has
    /// been signalled, in which case the caller must not open the stream at
    /// all: an admitted-then-aborted stream is the disconnect that
    /// quarantines the xAI auth.
    async fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> bool {
        let mut tasks = self.tasks.lock().await;
        // Checked under the lock that `wait` also takes, so a search cannot be
        // admitted after the drain has begun.
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task);
        true
    }

    /// Stops admitting searches. Called the moment shutdown is signalled, not
    /// after connections drain, so a search arriving mid-drain fails visibly
    /// instead of opening a stream nothing is left to finish.
    fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Waits for already-admitted search streams, bounded by `budget`. Runs
    /// after handlers drain and BEFORE the managed child is torn down, so
    /// in-flight streams end by themselves rather than by disconnection.
    async fn wait(&self, budget: std::time::Duration) {
        let drain = async {
            let mut tasks = self.tasks.lock().await;
            while tasks.join_next().await.is_some() {}
        };
        if tokio::time::timeout(budget, drain).await.is_err() {
            tracing::warn!(
                timeout_seconds = budget.as_secs_f64(),
                "xAI search drain expired; remaining streams are dropped"
            );
        }
    }
}

/// Answers a Grok-origin sub-call from xAI's hosted `web_search`, or returns
/// the failure to render. Strict by construction: this function never reaches
/// another vendor's backend.
async fn grok_native_websearch_response(
    state: &AppState,
    body: &Bytes,
    route: &crate::config::ModelRoute,
    subcall: &websearch::Subcall,
    capture: Option<RequestCapture>,
) -> Response {
    let started = std::time::Instant::now();
    let outcome = match gpt_forward_target(&state.cliproxy_upstream) {
        Some((base_url, credential)) => {
            xai_native_search(
                state,
                &base_url,
                credential.as_ref(),
                &route.upstream_model,
                subcall,
            )
            .await
        }
        None => Err(SearchFailure::GatewayUnavailable),
    };
    let input_tokens = estimate_input_tokens(body);
    let message = match outcome {
        Ok(links) => {
            tracing::info!(
                links = links.len(),
                origin = %route.routing_id,
                backend = "xai-web-search",
                elapsed_ms = started.elapsed().as_millis(),
                "answered routed-origin web search from xAI native search"
            );
            // No commentary text: the synthesized prose is never read, so it
            // can never leak into the result.
            websearch::synthesize_message(&route.routing_id, subcall, &links, "", input_tokens)
        }
        Err(failure) => {
            let (error_code, detail) = failure.rendered();
            tracing::warn!(
                origin = %route.routing_id,
                backend = "xai-web-search",
                %detail,
                "xAI native search failed; returning a failed-search result"
            );
            websearch::synthesize_error_message(
                &route.routing_id,
                subcall,
                error_code,
                &detail,
                input_tokens,
            )
        }
    };
    message_response(state, &message, subcall.stream, capture).await
}

/// One search against xAI's hosted `web_search` tool through the same
/// `CLIProxyAPI` child, streamed.
///
/// The stream is read by a task that owns it and always reads to the end, so
/// the connection is never closed early — a client disconnect makes the child
/// quarantine the xAI auth entry for 30–60s
/// (`auth_unavailable: no auth available (providers=xai)`), which would take
/// the user's whole Grok family offline after every search. This function only
/// waits for the sources (~3s), and giving up on that wait leaves the worker
/// running. Draining is measured not to block concurrent Grok traffic.
async fn xai_native_search(
    state: &AppState,
    base_url: &str,
    credential: Option<&headers::GptUpstreamCredential>,
    upstream_model: &str,
    subcall: &websearch::Subcall,
) -> Result<Vec<websearch::Link>, SearchFailure> {
    let policy =
        websearch::DomainPolicy::from_subcall(subcall).map_err(SearchFailure::InvalidFilter)?;
    let body = serde_json::to_vec(&websearch::xai_search_request_body(
        subcall,
        &policy,
        upstream_model,
    ))
    .map_err(|error| SearchFailure::Transport(error.into()))?;
    let limits = state.xai_searches.limits;
    let url = format!("{}/v1/responses", base_url.trim_end_matches('/'));
    let mut request = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .body(body);
    if let Some(credential) = credential {
        request = credential.apply(request);
    }
    let (harvested_tx, harvested_rx) = tokio::sync::oneshot::channel();
    // The registry takes the task BEFORE the stream is opened, so no stream
    // exists that it does not own — and none is opened once shutdown has been
    // signalled.
    let admitted = state
        .xai_searches
        .spawn(async move {
            xai_search_worker(request, policy, limits, harvested_tx).await;
        })
        .await;
    if !admitted {
        return Err(SearchFailure::ShuttingDown);
    }

    match tokio::time::timeout(limits.harvest_timeout, harvested_rx).await {
        Ok(Ok(result)) => result,
        // The worker ended without reporting: it panicked or was aborted.
        Ok(Err(_)) => Err(SearchFailure::NoSources("search task ended")),
        // Deadline: stop waiting, but leave the worker to finish the stream.
        Err(_) => Err(SearchFailure::TimedOut),
    }
}

/// Owns one search stream start to finish: reports the harvest as soon as it
/// appears, then keeps reading until the upstream ends the response.
async fn xai_search_worker(
    request: reqwest::RequestBuilder,
    policy: websearch::DomainPolicy,
    limits: crate::config::XaiSearchLimits,
    outcome_tx: tokio::sync::oneshot::Sender<Result<Vec<websearch::Link>, SearchFailure>>,
) {
    let mut outcome_tx = Some(outcome_tx);
    let mut report = |outcome| {
        if let Some(sender) = outcome_tx.take() {
            // The receiver is gone when the handler's deadline already
            // expired; the stream still gets drained below.
            let _ = sender.send(outcome);
        }
    };
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return report(Err(SearchFailure::Transport(error.into()))),
    };
    if response.status() != reqwest::StatusCode::OK {
        return report(Err(SearchFailure::Status(response.status().as_u16())));
    }
    let mut harvester = websearch::XaiSourceHarvester::new(policy);
    let mut stream = response.bytes_stream();
    let read = async {
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    tracing::debug!(%error, "xAI search stream ended early");
                    report(Err(SearchFailure::Transport(error.into())));
                    return;
                }
            };
            match harvester.push(&chunk) {
                Some(websearch::Harvested::Sources(links)) => report(Ok(links)),
                Some(websearch::Harvested::Ended(reason)) => {
                    report(Err(SearchFailure::NoSources(reason)));
                }
                None => {}
            }
            // Reading continues past the harvest on purpose: the response is
            // finished by the upstream, never by us.
        }
        report(Err(SearchFailure::NoSources("stream ended")));
    };
    if tokio::time::timeout(limits.drain_timeout, read)
        .await
        .is_err()
    {
        tracing::debug!("xAI search stream exceeded the drain budget");
    }
}

/// One search round-trip against the Codex search backend. Returns the
/// deduplicated links and the rendered search output. Empty results are not
/// an error (some query classes legitimately have no link results), but an
/// entirely empty response is.
async fn alpha_search(
    state: &AppState,
    base_url: &str,
    credential: Option<&headers::GptUpstreamCredential>,
    upstream_model: &str,
    subcall: &websearch::Subcall,
) -> anyhow::Result<(Vec<websearch::Link>, String)> {
    let url = format!("{}/v1/alpha/search", base_url.trim_end_matches('/'));
    // Mapped here rather than at the call sites: this function is the only
    // way to reach the Codex search backend, so a future caller cannot leak
    // a foreign slug to it by forgetting the conversion.
    let search_model = websearch::alpha_search_model(upstream_model);
    let body = serde_json::to_vec(&websearch::alpha_request_body(subcall, search_model))?;
    let mut request = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .timeout(std::time::Duration::from_secs(30));
    if let Some(credential) = credential {
        request = credential.apply(request);
    }
    let response = request.send().await?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "alpha search returned HTTP {}",
        response.status().as_u16()
    );
    let document = serde_json::from_slice::<serde_json::Value>(&response.bytes().await?)?;
    let output = document
        .get("output")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let links = document
        .get("results")
        .and_then(serde_json::Value::as_array)
        .map(|results| websearch::links_from_alpha_results(results))
        .unwrap_or_default();
    anyhow::ensure!(
        !links.is_empty() || !output.trim().is_empty(),
        "alpha search returned neither links nor output"
    );
    Ok((links, output))
}

/// Forwards the sub-call to the LLM upstream with streaming disabled and
/// buffers the complete message so its links can be repaired.
async fn legacy_websearch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: &Bytes,
    base_url: &str,
    credential: Option<&headers::GptUpstreamCredential>,
) -> anyhow::Result<serde_json::Value> {
    let mut document = serde_json::from_slice::<serde_json::Value>(rewritten)?;
    document["stream"] = serde_json::Value::Bool(false);
    let cache_headers = cache_identity_headers(parts, rewritten);
    let mut outgoing_headers = headers::request_headers(
        cache_headers.as_ref().unwrap_or(&parts.headers),
        true,
        true,
        credential,
    );
    // The body is parsed here, and the reqwest client does no decompression;
    // explicitly `identity` — an absent Accept-Encoding permits any coding.
    outgoing_headers.insert(
        header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
    let response = state
        .client
        .request(parts.method.clone(), upstream_url(base_url, &parts.uri))
        .headers(outgoing_headers)
        .body(serde_json::to_vec(&document)?)
        .timeout(std::time::Duration::from_mins(4))
        .send()
        .await?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "web search upstream returned HTTP {}",
        response.status().as_u16()
    );
    let message = serde_json::from_slice::<serde_json::Value>(&response.bytes().await?)?;
    anyhow::ensure!(
        message.get("type").and_then(serde_json::Value::as_str) == Some("message"),
        "web search upstream returned a non-message body"
    );
    Ok(message)
}

/// Serves a complete Anthropic message in the framing the client asked for:
/// the SSE event sequence for streaming requests, plain JSON otherwise.
async fn message_response(
    state: &AppState,
    message: &serde_json::Value,
    streaming: bool,
    capture: Option<RequestCapture>,
) -> Response {
    let mut response_headers = HeaderMap::new();
    let chunks = if streaming {
        response_headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        response_headers.insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        );
        websearch::message_to_sse(message)
    } else {
        response_headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        vec![Bytes::from(message.to_string())]
    };
    local_response(state, StatusCode::OK, response_headers, chunks, capture).await
}

async fn local_stub_response(
    state: &AppState,
    upstream_model: &str,
    request_body: &[u8],
    capture: Option<RequestCapture>,
) -> Response {
    let streaming = serde_json::from_slice::<serde_json::Value>(request_body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let (headers, chunks) = stub::response(upstream_model, streaming);
    local_response(state, StatusCode::OK, headers, chunks, capture).await
}

/// Ingress gate: with a token configured, only `/t/<token>/`-prefixed
/// requests are routed (the bare health endpoint stays reachable for the
/// `SessionStart` hook); the prefix is stripped before routing. Rejections
/// are generic 404s — no token material.
#[allow(clippy::result_large_err)] // the Err IS the HTTP response we return
fn apply_ingress_gate(
    config: &Config,
    parts: &mut axum::http::request::Parts,
) -> Result<(), Response> {
    let Some(token) = &config.ingress_token else {
        return Ok(());
    };
    let prefix = ingress_prefix(token);
    let path = parts.uri.path();
    if parts.method == Method::GET && path == HEALTH_PATH {
        return Ok(());
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
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "not found",
        ));
    };
    let rewritten = match parts.uri.query() {
        Some(query) => format!("{stripped}?{query}"),
        None => stripped,
    };
    let Ok(uri) = rewritten.parse::<Uri>() else {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "not found",
        ));
    };
    parts.uri = uri;
    Ok(())
}

/// Local liveness/version endpoint for the `SessionStart` hook and doctor.
fn health_response(state: &AppState) -> Response {
    let upstream = match &state.cliproxy_upstream {
        CliproxyUpstream::Stub => "stub",
        CliproxyUpstream::External { .. } => "external",
        CliproxyUpstream::ManagedUnavailable => "unavailable",
        CliproxyUpstream::Managed(handle) => {
            if handle.is_ready() {
                "ready"
            } else {
                "not-ready"
            }
        }
    };
    let status = if matches!(upstream, "not-ready" | "unavailable") {
        "degraded"
    } else {
        "ok"
    };
    let bytes = Bytes::from(
        json!({
            "status": status,
            "version": env!("CARGO_PKG_VERSION"),
            "cliproxy-upstream": upstream,
            // What this process resolved at startup. Doctor compares it with
            // a fresh read so a settings change that the running service has
            // not picked up is visible rather than silently mis-scaling.
            "declared-context-window": state.config.declared_context_window,
            // Deprecated duplicate for the auto-update skew window: a pre-0.1.3
            // doctor probing a newer service still reads the old key. Remove
            // after one release.
            "codex-upstream": upstream,
        })
        .to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    build_response(StatusCode::OK, headers, Body::from(bytes))
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
        // The sniffer and the overflow-error translator parse raw response
        // bytes and the reqwest client does no decompression, so GPT-branch
        // and tapped responses must be identity-encoded. Explicitly
        // `identity`, not merely absent — an absent Accept-Encoding permits
        // the server to pick any coding. Loopback traffic; compression buys
        // nothing here.
        outgoing_headers.insert(
            header::ACCEPT_ENCODING,
            axum::http::HeaderValue::from_static("identity"),
        );
    }
    if strip_credentials && let Some(request_capture) = &mut capture {
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

async fn models_response(
    state: &AppState,
    inbound_headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: Bytes,
    capture: Option<RequestCapture>,
) -> Response {
    let url = upstream_url(&state.config.anthropic_upstream_base, uri);
    let mut outgoing_headers = headers::request_headers(inbound_headers, false, false, None);
    // This path parses the upstream body (to merge routed GPT models in),
    // and the reqwest client does no decompression — request an identity
    // response explicitly (absence would permit any coding).
    outgoing_headers.insert(
        header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
    let response = match state
        .client
        .request(method.clone(), url)
        .headers(outgoing_headers)
        .body(body)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "Anthropic models request failed; returning routed models only");
            return models_fallback_response(state, capture).await;
        }
    };

    if response.status() != reqwest::StatusCode::OK {
        tracing::warn!(
            status = response.status().as_u16(),
            "Anthropic models request was not successful; returning routed models only"
        );
        return models_fallback_response(state, capture).await;
    }

    let response_headers = headers::response_headers(response.headers(), true);
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "failed to read Anthropic models response; returning routed models only");
            return models_fallback_response(state, capture).await;
        }
    };
    let mut document = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(document) => document,
        Err(error) => {
            tracing::warn!(%error, "failed to parse Anthropic models response; returning routed models only");
            return models_fallback_response(state, capture).await;
        }
    };
    let Some(data) = document
        .get_mut("data")
        .and_then(serde_json::Value::as_array_mut)
    else {
        tracing::warn!("Anthropic models response has no data array; returning routed models only");
        return models_fallback_response(state, capture).await;
    };
    let mut model_ids = data
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<HashSet<_>>();
    for route in state.config.effective_models() {
        if model_ids.insert(route.routing_id.clone()) {
            data.push(json!({
                "id": route.routing_id,
                "display_name": route.display_name,
                "type": "model"
            }));
        }
    }
    let merged = Bytes::from(serde_json::to_vec(&document).expect("JSON values always serialize"));
    local_response(
        state,
        StatusCode::OK,
        response_headers,
        vec![merged],
        capture,
    )
    .await
}

async fn models_fallback_response(state: &AppState, capture: Option<RequestCapture>) -> Response {
    let document = json!({
        "data": state.config.effective_models().map(|route| json!({
            "id": route.routing_id,
            "display_name": route.display_name,
            "type": "model"
        })).collect::<Vec<_>>()
    });
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    local_response(
        state,
        StatusCode::OK,
        response_headers,
        vec![Bytes::from(document.to_string())],
        capture,
    )
    .await
}

async fn upstream_response(
    state: &AppState,
    response: reqwest::Response,
    capture: Option<RequestCapture>,
    policies: Option<GptPolicies>,
    tap: Option<SniffTap>,
) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16()).expect("valid HTTP status");
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
        if let Some(tap) = tap.filter(|_| status == StatusCode::OK) {
            // Passive tee: parse each chunk and commit any completed
            // WebSearch observations BEFORE yielding it, so the client can
            // never issue the follow-up sub-call ahead of the pending entry.
            // Bytes are forwarded unchanged whether or not parsing succeeds.
            let mut sniffer = websearch::ToolUseSniffer::new(is_sse);
            Box::pin(async_stream::stream! {
                let mut inner = Box::pin(stream);
                while let Some(item) = inner.next().await {
                    if let Ok(bytes) = &item {
                        for search in sniffer.push(bytes) {
                            tap.pending.insert(
                                websearch::PendingKey::new(
                                    tap.session_id.clone(),
                                    &search.query,
                                    search.allowed_domains.as_deref(),
                                    search.blocked_domains.as_deref(),
                                ),
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

    if let Some(policies) = policies.filter(|_| transform_usage) {
        let transformed_stream = async_stream::stream! {
            let mut stream = Box::pin(stream);
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
        };
        let body = streaming_response_body(
            state,
            status,
            &response_headers,
            capture,
            transformed_stream,
        );
        return build_response(status, response_headers, body);
    }

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
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    local_response(state, status, headers, vec![bytes], capture).await
}

fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    let bytes = Bytes::from(
        json!({"type":"error","error":{"type":error_type,"message":message}}).to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    build_response(status, headers, Body::from(bytes))
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
    async fn search_admission_closes_at_shutdown_and_running_streams_are_awaited() {
        let tasks = XaiSearchTasks::new(crate::config::XaiSearchLimits::default());
        let running = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // A search admitted before the signal runs, and shutdown waits for it.
        let (started, done) = (running.clone(), finished.clone());
        assert!(
            tasks
                .spawn(async move {
                    started.notify_one();
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    done.store(true, std::sync::atomic::Ordering::SeqCst);
                })
                .await
        );
        running.notified().await;

        tasks.close();
        // A search arriving after the signal is refused, and its work never
        // runs: an admitted-then-aborted stream is the quarantine hazard, so
        // the stream must not be opened at all.
        let rejected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran = rejected.clone();
        assert!(
            !tasks
                .spawn(async move {
                    ran.store(true, std::sync::atomic::Ordering::SeqCst);
                })
                .await
        );

        assert!(!finished.load(std::sync::atomic::Ordering::SeqCst));
        tasks.wait(std::time::Duration::from_secs(5)).await;
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "shutdown returned before the admitted stream finished"
        );
        assert!(
            !rejected.load(std::sync::atomic::Ordering::SeqCst),
            "a refused search still ran"
        );
    }

    #[test]
    fn upstream_url_preserves_encoded_path_and_query_text() {
        let uri: Uri = "/v1/messages?beta=true&raw=%2F&empty=".parse().unwrap();
        assert_eq!(
            upstream_url("http://127.0.0.1:9000/", &uri),
            "http://127.0.0.1:9000/v1/messages?beta=true&raw=%2F&empty="
        );
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
        // Deprecated duplicate key, kept for one release for update-skew
        // tolerance (see health_response).
        assert_eq!(health["codex-upstream"], "unavailable");
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
            Arc::new(XaiSearchTasks::new(
                crate::config::XaiSearchLimits::default(),
            )),
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
                "codex".to_string(),
                UpstreamConfig {
                    mode: UpstreamMode::Stub,
                    ..UpstreamConfig::default()
                },
            )]),
            models: vec![ModelRoute {
                routing_id: "claude-gpt-test".to_string(),
                upstream: "codex".to_string(),
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
