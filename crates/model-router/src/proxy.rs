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
use crate::routing::{Branch, RoutingDecision, decide, substitute_model};
use crate::stub;
use crate::usage::{SseUsageTransformer, estimate_input_tokens};
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
        Ok(Self {
            config: Arc::new(config),
            client,
            capture,
            cliproxy_upstream,
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
    let app = app_with(config, managed).await?;
    serve_app(listener, app, shutdown, drain_timeout).await?;
    Ok(())
}

async fn serve_app(
    listener: TcpListener,
    app: Router,
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
            let _ = graceful_tx.send(());
            if let Ok(result) = tokio::time::timeout(drain_timeout, &mut server).await {
                result
            } else {
                tracing::warn!(
                    timeout_seconds = drain_timeout.as_secs_f64(),
                    "shutdown drain expired; dropping in-flight connections"
                );
                Ok(())
            }
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
    let state = AppState::new(config, managed).await?;
    Ok(Router::new().fallback(any(handle)).with_state(state))
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
        Branch::Claude => {
            forward(
                &state,
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
            )
            .await
        }
        Branch::Gpt => gpt_response(&state, &parts, body, &decision, capture).await,
    }
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
    let rewritten = match substitute_model(&body, &route.upstream_model) {
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
    let estimated_input_tokens = estimate_input_tokens(&rewritten);

    if state.config.web_search.mode != WebSearchMode::Off
        && parts.method == Method::POST
        && parts.uri.path() == "/v1/messages"
        && let Some((base_url, credential)) = gpt_forward_target(&state.cliproxy_upstream)
        && let Some(subcall) = websearch::detect(&rewritten)
    {
        return websearch_response(
            state,
            parts,
            rewritten,
            &route.routing_id,
            &route.upstream_model,
            &subcall,
            &base_url,
            credential.as_ref(),
            estimated_input_tokens,
            capture,
        )
        .await;
    }

    forward_gpt(
        state,
        parts,
        rewritten,
        &route.upstream_model,
        estimated_input_tokens,
        capture,
    )
    .await
}

/// Sends an already-rewritten GPT-branch request to the configured cliproxy
/// upstream (or the stub / an actionable local error).
async fn forward_gpt(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: Bytes,
    upstream_model: &str,
    estimated_input_tokens: u64,
    capture: Option<RequestCapture>,
) -> Response {
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
                &parts.headers,
                rewritten,
                true,
                true,
                credential.as_ref(),
                Some(estimated_input_tokens),
                capture,
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
                &parts.headers,
                rewritten,
                true,
                true,
                Some(&handle.credential),
                Some(estimated_input_tokens),
                capture,
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
    estimated_input_tokens: u64,
    capture: Option<RequestCapture>,
) -> Response {
    if state.config.web_search.mode == WebSearchMode::Alpha {
        match alpha_search(state, base_url, credential, upstream_model, subcall).await {
            Ok((links, output)) => {
                tracing::info!(links = links.len(), "answered web search from alpha/search");
                let message = websearch::synthesize_message(
                    routing_id,
                    subcall,
                    &links,
                    &output,
                    estimated_input_tokens,
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
            if let Some(input_tokens) = message
                .get_mut("usage")
                .and_then(|usage| usage.get_mut("input_tokens"))
                && input_tokens.as_u64() == Some(0)
            {
                *input_tokens = serde_json::Value::from(estimated_input_tokens);
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
                estimated_input_tokens,
                capture,
            )
            .await
        }
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
    let body = serde_json::to_vec(&websearch::alpha_request_body(subcall, upstream_model))?;
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
    let mut outgoing_headers = headers::request_headers(&parts.headers, true, true, credential);
    // The body is parsed here, and the reqwest client does no decompression.
    outgoing_headers.remove(header::ACCEPT_ENCODING);
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
    estimated_input_tokens: Option<u64>,
    mut capture: Option<RequestCapture>,
) -> Response {
    let url = upstream_url(upstream_base, uri);
    let outgoing_headers = headers::request_headers(
        inbound_headers,
        strip_credentials,
        body_changed,
        gpt_credential,
    );
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
        Ok(response) => upstream_response(state, response, capture, estimated_input_tokens),
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
    // This is the one path that parses the upstream body (to merge routed GPT
    // models in), and the reqwest client does no decompression — request an
    // identity response instead of forwarding the client's accept-encoding.
    outgoing_headers.remove(header::ACCEPT_ENCODING);
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

fn upstream_response(
    state: &AppState,
    response: reqwest::Response,
    capture: Option<RequestCapture>,
    estimated_input_tokens: Option<u64>,
) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16()).expect("valid HTTP status");
    let transform_usage = estimated_input_tokens.is_some_and(|_| {
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(';').next().is_some_and(|media_type| {
                    media_type.trim().eq_ignore_ascii_case("text/event-stream")
                })
            })
    });
    let response_headers = headers::response_headers(response.headers(), transform_usage);
    let stream = response.bytes_stream();

    if let Some(estimated_input_tokens) = estimated_input_tokens.filter(|_| transform_usage) {
        let transformed_stream = async_stream::stream! {
            let mut stream = Box::pin(stream);
            let mut transformer = SseUsageTransformer::new(estimated_input_tokens);
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
