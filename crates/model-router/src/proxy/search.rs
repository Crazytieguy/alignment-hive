//! The proxy's answers to Claude Code's `WebSearch` sub-call
//! ([`crate::websearch`] recognizes it and builds the replies). A sub-call is
//! steered to the backend of the agent that asked for the search, whichever
//! branch carried it. Grok and Muse origins are answered strictly by their
//! vendor's hosted search (a failure is a failed-search result, never another
//! vendor), even with `[web-search] mode = "off"`. A
//! Claude origin on the GPT branch is tried at Anthropic first and, on a
//! transport error, 429 or 5xx, continues to the GPT path. Other routed
//! origins go to the Codex alpha search in `alpha` mode, then the scrape
//! path. Whatever none of these answers falls through to the branch's plain
//! forward.

use std::future::Future;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;

use super::{
    AppState, ForwardTarget, cache_identity_headers, forward_gpt, forward_to_anthropic,
    frame_message, gpt_forward_target, local_response, login_refusal, require_identity_encoding,
    upstream_url,
};
use crate::capture::RequestCapture;
use crate::config::WebSearchMode;
use crate::headers;
use crate::routing::substitute_model;
use crate::usage::{GptPolicies, estimate_input_tokens};
use crate::websearch;

/// How long a buffered sub-call forward may take end to end.
const SUBCALL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(4);

/// These events log under the parent module's target, so their lines read
/// `model_router::proxy` like the rest of the proxy's.
const LOG_TARGET: &str = "model_router::proxy";

/// A sub-call arriving on the Claude branch whose `WebSearch` was invoked by
/// a routed agent (GPT, Grok, Muse, or open-weights): answer from the
/// matching backend. `alpha` mode asks the Codex search backend and, on failure,
/// falls back to the origin route's own scrape path — never straight to
/// Anthropic, which would silently move the work and the bill to a vendor
/// the user did not choose. `scrape` mode goes to that same path directly.
/// The native Anthropic passthrough is the last resort only.
pub(super) async fn gpt_origin_on_claude_branch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: Bytes,
    routing_id: &str,
    subcall: &websearch::Subcall,
    mut capture: Option<RequestCapture>,
) -> Response {
    let route = state.config.route(routing_id);
    // A Grok or Muse origin is served by its own vendor's search, strictly:
    // no alpha, no scrape, no Anthropic — not even when the gateway is
    // unavailable. An origin whose route has since left the config has no
    // known family, so it keeps the existing behaviour below.
    if let Some((route, native)) =
        route.and_then(|route| Some((route, websearch::native_search(route.family)?)))
    {
        return native_websearch_response(state, &body, route, native, subcall, capture).await;
    }
    // The ingress gate saw only this Claude carrier, so the origin's route
    // passes it here, as the native arm's search does: a refusal is a
    // failed-search result, never Anthropic.
    if let Some(route) = route
        && let Some(message) = login_refusal(state, Some(route)).await
    {
        let failed = websearch::synthesize_error_message(
            &route.routing_id,
            subcall,
            websearch::search_error::UNAVAILABLE,
            &message,
            estimate_input_tokens(&body),
        );
        return message_response(state, &failed, subcall.stream, capture).await;
    }
    // A forced search may carry the caller's full tool set alongside
    // `web_search` (see `websearch::detect`), and `legacy_websearch` forwards
    // it, so this GPT-bound body needs the tool rewrites too, and the origin's
    // service tier — identity it does not: the side call's own system prompt
    // is not a conversation.
    match (route, gpt_forward_target(&state.cliproxy_upstream)) {
        (Some(route), Some(target)) => match substitute_model(&body, &route.upstream_model)
            .and_then(|body| crate::tool_changes::portable_tools_in_body(&body, route.family))
            .and_then(|body| at_tier(&body, route.fast_tier))
        {
            Ok(rewritten) => {
                match gpt_backend_answer(
                    state,
                    parts,
                    &Bytes::from(rewritten),
                    route,
                    subcall,
                    &target,
                    capture,
                )
                .await
                {
                    Ok(response) => return response,
                    Err(returned) => capture = returned,
                }
            }
            Err(error) => {
                tracing::warn!(target: LOG_TARGET, %error, "failed to rewrite sub-call model");
            }
        },
        (Some(_), None) => tracing::warn!(
            target: LOG_TARGET,
            origin = routing_id,
            "no GPT upstream to answer a routed-origin web search; answering from Anthropic"
        ),
        (None, _) => {}
    }
    forward_to_anthropic(state, parts, body, capture, None).await
}

/// A `WebSearch` sub-call that arrived on the GPT branch: answered by the
/// origin's own backend, else through the alpha/scrape ladder, else
/// forwarded to the GPT upstream. With search `off`, only the vendor-native
/// answer remains; everything else is forwarded untouched.
///
/// The scrape path and the forward run at a GPT origin's service tier, not
/// the carrier's, so a standard agent's search on a `-fast` carrier spends
/// no priority and a twin's search keeps it; the model and attribution stay
/// the carrier's. A Claude or unrecorded origin keeps the carrier's tier,
/// and `off` records no GPT origin, so its forward stays the carrier's.
#[allow(clippy::too_many_arguments)]
pub(super) async fn gpt_branch_subcall(
    state: &AppState,
    parts: &axum::http::request::Parts,
    body: &Bytes,
    rewritten: Bytes,
    route: &crate::config::ModelRoute,
    subcall: &websearch::Subcall,
    policies: GptPolicies,
    mut capture: Option<RequestCapture>,
) -> Response {
    let origin = state.origin_of(&rewritten, subcall);
    // Policy lives in `websearch::native_search_route` (unit-tested there);
    // this only supplies the config lookup.
    let native_search_route =
        websearch::native_search_route(origin.as_ref(), route, |routing_id| {
            state.config.route(routing_id)
        });
    // The Anthropic-native path needs no GPT upstream — attempt it even when
    // the GPT target is unavailable.
    if let Some(origin) = &origin
        && state.intercepts(origin)
        && let websearch::Origin::Claude { model } = origin
    {
        match claude_origin_on_gpt_branch(state, parts, body, model, subcall, capture).await {
            Ok(response) => return response,
            Err(returned) => capture = returned,
        }
    }
    if let Some((route, native)) = native_search_route {
        return native_websearch_response(state, &rewritten, route, native, subcall, capture).await;
    }
    // Dropping the tier is always safe; asking for it only on a carrier that
    // has one (never Grok, Muse or an openai-compat host).
    let fast = match &origin {
        Some(websearch::Origin::Gpt { routing_id }) => state.config.route(routing_id),
        _ => None,
    }
    .map_or(route.fast_tier, |origin| {
        origin.fast_tier && crate::config::has_fast_tier(route)
    });
    let rewritten = if fast == route.fast_tier {
        rewritten
    } else {
        // `rewrite_gpt_body` wrote it, so it parses.
        at_tier(&rewritten, fast).map_or(rewritten, Bytes::from)
    };
    // Past the native arm no origin searches natively, so only `[web-search]
    // mode` decides, as `AppState::intercepts` would for any origin.
    if state.config.web_search.mode != WebSearchMode::Off
        && let Some(target) = gpt_forward_target(&state.cliproxy_upstream)
    {
        match gpt_backend_answer(state, parts, &rewritten, route, subcall, &target, capture).await {
            Ok(response) => return response,
            Err(returned) => capture = returned,
        }
    }
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

/// A sub-call arriving on the GPT branch whose `WebSearch` was invoked by a
/// Claude agent: answer it from Anthropic natively. `Err` returns the
/// capture record so the caller can continue with the GPT path (transport /
/// transient failures only; caller-error statuses are surfaced).
#[allow(clippy::result_large_err)] // the Err returns the capture for the passthrough fallback
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
            tracing::info!(
                target: LOG_TARGET,
                origin = %origin_model,
                "answered Claude-origin web search from Anthropic"
            );
            Ok(message_response(state, &message, subcall.stream, capture).await)
        }
        NativeOutcome::Surface(status, response_headers, body) => {
            Ok(local_response(state, status, response_headers, vec![body], capture).await)
        }
        NativeOutcome::Fallback(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "Anthropic-native web search failed; using the GPT path"
            );
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
    require_identity_encoding(&mut outgoing_headers);
    let result = state
        .client
        .request(
            parts.method.clone(),
            upstream_url(&state.config.anthropic_upstream_base, &parts.uri),
        )
        .headers(outgoing_headers)
        .body(body)
        .timeout(SUBCALL_TIMEOUT)
        .send()
        .await;
    let response = match result {
        Ok(response) => response,
        Err(error) => return NativeOutcome::Fallback(error.into()),
    };
    let status = response.status();
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
            tracing::warn!(
                target: LOG_TARGET,
                "Anthropic returned HTTP 200 with a non-message body; surfacing it"
            );
            NativeOutcome::Surface(status, response_headers, bytes)
        }
    }
}

/// Answers a `WebSearch` sub-call from the GPT backend behind `target`: the
/// Codex search backend in `alpha` mode, else (or when that fails) the
/// route's own LLM upstream with links scraped into the empty result blocks.
/// Shared by both branches so the two degrade identically — the sub-call
/// stays with the vendor the user picked for the work. `Err` returns the
/// capture so the caller can continue to its own last resort.
///
/// Sub-call responses are conversation context for nobody, so they carry
/// the input estimate but never the route's scale.
#[allow(clippy::result_large_err)] // the Err returns the capture for the fallback
async fn gpt_backend_answer(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: &Bytes,
    route: &crate::config::ModelRoute,
    subcall: &websearch::Subcall,
    target: &ForwardTarget,
    capture: Option<RequestCapture>,
) -> Result<Response, Option<RequestCapture>> {
    if state.config.web_search.mode == WebSearchMode::Alpha {
        match alpha_search(state, target, &route.upstream_model, subcall).await {
            Ok((links, output)) => {
                tracing::info!(
                    target: LOG_TARGET,
                    links = links.len(),
                    origin = %route.routing_id,
                    backend = "alpha-search",
                    "answered web search from alpha/search"
                );
                let message = websearch::synthesize_message(
                    &route.routing_id,
                    subcall,
                    &links,
                    &output,
                    estimate_input_tokens(rewritten),
                );
                return Ok(message_response(state, &message, subcall.stream, capture).await);
            }
            Err(error) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    %error,
                    "alpha web search failed; falling back to the LLM web search path"
                );
            }
        }
    }
    match legacy_websearch(state, parts, rewritten, target).await {
        Ok(mut message) => {
            if let Some(usage) = message.get_mut("usage") {
                crate::usage::inject_estimated_input_tokens(
                    usage,
                    estimate_input_tokens(rewritten),
                );
            }
            let filled = websearch::fill_empty_web_search_results(&mut message);
            tracing::info!(
                target: LOG_TARGET,
                filled,
                origin = %route.routing_id,
                "scraped links into the LLM web search response"
            );
            Ok(message_response(state, &message, subcall.stream, capture).await)
        }
        Err(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "buffered web search forward failed; passing the sub-call through"
            );
            Err(capture)
        }
    }
}

/// Why a vendor-native search could not be answered. Carries its own mapping
/// to the client-visible failed-search result, so the taxonomy lives in one
/// place.
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
    /// The login gate refuses the route's requests right now; the reason names
    /// the fix ([`login_refusal`]).
    Refused(String),
}

impl SearchFailure {
    /// `(error_code, detail)` for [`websearch::synthesize_error_message`].
    fn rendered(&self, native: websearch::NativeSearch) -> (&'static str, String) {
        let vendor = native.vendor();
        match self {
            Self::Status(429) => (
                websearch::search_error::TOO_MANY_REQUESTS,
                format!("{vendor} search is rate-limited"),
            ),
            Self::Status(status) => (
                websearch::search_error::UNAVAILABLE,
                format!("{vendor} search returned HTTP {status}"),
            ),
            Self::Transport(error) => (
                websearch::search_error::UNAVAILABLE,
                format!("{vendor} search could not be reached ({error})"),
            ),
            Self::TimedOut => (
                websearch::search_error::UNAVAILABLE,
                format!("{vendor} search did not return sources in time"),
            ),
            Self::NoSources(reason) => (
                websearch::search_error::UNAVAILABLE,
                format!("{vendor} search returned no sources ({reason})"),
            ),
            Self::Refused(message) => (websearch::search_error::UNAVAILABLE, message.clone()),
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

/// Owns every in-flight vendor-native search stream.
///
/// Each search runs in a task that reads its response to completion no matter
/// what the request handler does, because dropping a live stream disconnects
/// the client mid-response and makes the child quarantine the xAI auth entry
/// for 30–60s (measured). Meta searches are drained the same way: whether the
/// child quarantines a Meta login too is unmeasured, and with
/// `max_tool_calls: 1` the rest of the stream costs ~2s. The handler's
/// deadline therefore stops it WAITING; it never cancels the HTTP read.
/// Shutdown stops admitting new searches and waits for the running ones, so a
/// restart cannot orphan a stream either.
///
/// How many searches may run at once is not the router's business: a parallel
/// sweep should meet the limits the user's own subscription imposes, not an
/// invented one. Admission is a shutdown gate, nothing more.
pub struct SearchStreams {
    closed: std::sync::atomic::AtomicBool,
    tasks: tokio::sync::Mutex<tokio::task::JoinSet<()>>,
}

impl SearchStreams {
    pub(super) fn new() -> Self {
        Self {
            closed: std::sync::atomic::AtomicBool::new(false),
            tasks: tokio::sync::Mutex::new(tokio::task::JoinSet::new()),
        }
    }

    /// Takes ownership of one search stream's task. `false` once shutdown has
    /// been signalled, in which case the caller must not open the stream at
    /// all.
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
    pub(super) fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Waits for already-admitted search streams, bounded by `budget`. Runs
    /// after handlers drain and BEFORE the managed child is torn down.
    pub(super) async fn wait(&self, budget: std::time::Duration) {
        let drain = async {
            let mut tasks = self.tasks.lock().await;
            while tasks.join_next().await.is_some() {}
        };
        if tokio::time::timeout(budget, drain).await.is_err() {
            tracing::warn!(
                target: LOG_TARGET,
                timeout_seconds = budget.as_secs_f64(),
                "search drain expired; remaining streams are dropped"
            );
        }
    }
}

/// Answers a Grok- or Muse-origin sub-call from the origin family's own
/// hosted `web_search`, or returns the failure to render. Strict by
/// construction: this function never reaches another vendor's backend.
async fn native_websearch_response(
    state: &AppState,
    body: &Bytes,
    route: &crate::config::ModelRoute,
    native: websearch::NativeSearch,
    subcall: &websearch::Subcall,
    capture: Option<RequestCapture>,
) -> Response {
    let started = std::time::Instant::now();
    let outcome = match gpt_forward_target(&state.cliproxy_upstream) {
        Some(target) => native_search(state, &target, route, native, subcall).await,
        None => Err(SearchFailure::GatewayUnavailable),
    };
    let input_tokens = estimate_input_tokens(body);
    let message = match outcome {
        Ok(links) => {
            tracing::info!(
                target: LOG_TARGET,
                links = links.len(),
                origin = %route.routing_id,
                backend = native.backend(),
                elapsed_ms = started.elapsed().as_millis(),
                "answered routed-origin web search from the vendor's native search"
            );
            // No commentary text: the synthesized prose is never read, so it
            // can never leak into the result.
            websearch::synthesize_message(&route.routing_id, subcall, &links, "", input_tokens)
        }
        Err(failure) => {
            let (error_code, detail) = failure.rendered(native);
            tracing::warn!(
                target: LOG_TARGET,
                origin = %route.routing_id,
                backend = native.backend(),
                %detail,
                "native search failed; returning a failed-search result"
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

/// One search against the vendor's hosted `web_search` tool through the same
/// `CLIProxyAPI` child, streamed. This function only waits for the sources
/// (~2–3s); the stream itself is owned by a task that reads it to the end
/// whatever happens here (see [`SearchStreams`]).
///
/// A Meta search passes the same Muse gate as a Muse turn first: the
/// sub-call that carries it may be a Claude or GPT request the gate never
/// saw.
async fn native_search(
    state: &AppState,
    target: &ForwardTarget,
    route: &crate::config::ModelRoute,
    native: websearch::NativeSearch,
    subcall: &websearch::Subcall,
) -> Result<Vec<websearch::Link>, SearchFailure> {
    if let Some(message) = login_refusal(state, Some(route)).await {
        return Err(SearchFailure::Refused(message));
    }
    let policy =
        websearch::DomainPolicy::from_subcall(subcall).map_err(SearchFailure::InvalidFilter)?;
    let body = serde_json::to_vec(&native.request_body(subcall, &policy, &route.upstream_model))
        .expect("JSON values always serialize");
    let limits = state.config.native_search;
    let url = format!("{}/v1/responses", target.base_url.trim_end_matches('/'));
    let mut request = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .header(header::ACCEPT_ENCODING, "identity")
        .body(body);
    if let Some(credential) = &target.credential {
        request = credential.apply(request);
    }
    let (harvested_tx, harvested_rx) = tokio::sync::oneshot::channel();
    // The registry takes the task BEFORE the stream is opened, so no stream
    // exists that it does not own — and none is opened once shutdown has been
    // signalled.
    let admitted = state
        .search_streams
        .spawn(async move {
            search_worker(request, native, policy, limits, harvested_tx).await;
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
async fn search_worker(
    request: reqwest::RequestBuilder,
    native: websearch::NativeSearch,
    policy: websearch::DomainPolicy,
    limits: crate::config::NativeSearchLimits,
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
    let mut harvester = websearch::SourceHarvester::new(policy);
    let mut stream = response.bytes_stream();
    let read = async {
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    tracing::debug!(target: LOG_TARGET, %error, backend = native.backend(), "search stream ended early");
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
        tracing::warn!(
            target: LOG_TARGET,
            budget_seconds = limits.drain_timeout.as_secs(),
            backend = native.backend(),
            "search stream exceeded the drain budget and was dropped; the child may \
             quarantine the login"
        );
    }
}

/// One search round-trip against the Codex search backend. Returns the
/// deduplicated links and the rendered search output. Empty results are not
/// an error (some query classes legitimately have no link results), but an
/// entirely empty response is.
async fn alpha_search(
    state: &AppState,
    target: &ForwardTarget,
    upstream_model: &str,
    subcall: &websearch::Subcall,
) -> anyhow::Result<(Vec<websearch::Link>, String)> {
    let url = format!("{}/v1/alpha/search", target.base_url.trim_end_matches('/'));
    let body = serde_json::to_vec(&websearch::alpha_request_body(subcall, upstream_model))
        .expect("JSON values always serialize");
    let mut request = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT_ENCODING, "identity")
        .body(body)
        .timeout(std::time::Duration::from_secs(30));
    if let Some(credential) = &target.credential {
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
        .filter(|links| !links.is_empty())
        .unwrap_or_else(|| websearch::links_from_alpha_output(&output));
    anyhow::ensure!(
        !links.is_empty() || !output.trim().is_empty(),
        "alpha search returned neither links nor output"
    );
    Ok((links, output))
}

/// `body` at the requested service tier ([`crate::service_tier`]): a search
/// sub-call runs at the tier of the agent that asked for it.
fn at_tier(body: &[u8], fast: bool) -> anyhow::Result<Vec<u8>> {
    let mut document = serde_json::from_slice::<serde_json::Value>(body)?;
    crate::service_tier::apply(&mut document, fast);
    Ok(serde_json::to_vec(&document)?)
}

/// Forwards the sub-call to the LLM upstream with streaming disabled and
/// buffers the complete message so its links can be repaired.
async fn legacy_websearch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    rewritten: &Bytes,
    target: &ForwardTarget,
) -> anyhow::Result<serde_json::Value> {
    let mut document = serde_json::from_slice::<serde_json::Value>(rewritten)?;
    document["stream"] = serde_json::Value::Bool(false);
    let cache_headers = cache_identity_headers(parts, rewritten);
    let mut outgoing_headers = headers::request_headers(
        cache_headers.as_ref().unwrap_or(&parts.headers),
        true,
        true,
        target.credential.as_ref(),
    );
    require_identity_encoding(&mut outgoing_headers);
    let response = state
        .client
        .request(
            parts.method.clone(),
            upstream_url(&target.base_url, &parts.uri),
        )
        .headers(outgoing_headers)
        .body(serde_json::to_vec(&document)?)
        .timeout(SUBCALL_TIMEOUT)
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
    let (response_headers, chunks) = frame_message(message, streaming);
    local_response(state, StatusCode::OK, response_headers, chunks, capture).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use std::sync::Arc;

    #[tokio::test]
    async fn search_admission_closes_at_shutdown_and_running_streams_are_awaited() {
        let tasks = SearchStreams::new();
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

    /// Serves `/v1/messages` as a response calling `WebSearch` and records
    /// every path it is asked for; anything else is a 500.
    async fn fake_child(paths: Arc<std::sync::Mutex<Vec<String>>>) -> String {
        let events = [
            serde_json::json!({"type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"toolu_1","name":"WebSearch","input":{}}}),
            serde_json::json!({"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta",
                         "partial_json":"{\"query\":\"bun release notes\"}"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
        ]
        .map(|data| format!("event: x\ndata: {data}\n\n"))
        .concat();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let (paths, events) = (paths.clone(), events.clone());
                async move {
                    let path = request.uri().path().to_string();
                    paths.lock().unwrap().push(path.clone());
                    if path == "/v1/messages" {
                        ([(header::CONTENT_TYPE, "text/event-stream")], events).into_response()
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    async fn send(app: &axum::Router, body: serde_json::Value) -> (StatusCode, String) {
        use tower::ServiceExt;
        let request = axum::http::Request::post("/v1/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// The sub-call carrying a routed agent's search may be a Claude request
    /// the login gate never refused, so the origin's route passes the gate
    /// before either search backend: a login that undoes an exclusion the
    /// origin relies on, appearing after the agent's turn, makes the search a
    /// failed result that reaches neither the child nor Anthropic.
    async fn assert_claude_carried_search_refused(
        mut config: crate::config::Config,
        routing_id: &str,
        login: (&str, &str),
        needle: &str,
    ) {
        let auth = tempfile::tempdir().unwrap();
        let child_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let anthropic_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let child = fake_child(child_paths.clone()).await;
        config.anthropic_upstream_base = fake_child(anthropic_paths.clone()).await;
        config.auth_dir = Some(auth.path().to_path_buf());
        config.models = vec![crate::config::ModelRoute {
            routing_id: routing_id.to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: routing_id.to_string(),
            display_name: routing_id.to_string(),
            ..Default::default()
        }];
        let handle = crate::supervisor::ManagedHandle::ready_for_tests(child);
        let (app, _) = super::super::build(config, Some(handle)).await.unwrap();
        let metadata = serde_json::json!({"user_id":
            r#"{"device_id":"d","account_uuid":"a","session_id":"sess-1"}"#});
        let turn = serde_json::json!({
            "model": routing_id, "max_tokens": 64, "stream": true, "metadata": metadata,
            "messages": [{"role": "user", "content": "find bun release notes"}],
            "tools": [{"name": "WebSearch", "description": "Search the web",
                       "input_schema": {"type": "object"}}],
        });
        let (status, body) = send(&app, turn).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("content_block_stop"), "{body}");

        std::fs::write(auth.path().join(login.0), login.1).unwrap();
        let subcall = serde_json::json!({
            "model": "claude-sonnet-4-5", "max_tokens": 4096, "stream": true,
            "metadata": metadata,
            "messages": [{"role": "user", "content": [{"type": "text",
                "text": "Perform a web search for the query: bun release notes"}]}],
            "system": [{"type": "text",
                "text": "You are an assistant for performing a web search tool use"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
        });
        let (status, body) = send(&app, subcall).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("web_search_tool_result_error"), "{body}");
        assert!(body.contains("model-router refused this request"), "{body}");
        assert!(body.contains(needle), "{body}");
        assert_eq!(*child_paths.lock().unwrap(), ["/v1/messages"]);
        assert!(anthropic_paths.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_claude_carried_gpt_search_is_refused_while_a_codex_login_lists_exclusions() {
        let mut config = crate::config::Config::default();
        config.gpt.auth = crate::config::GptAuth::ApiKey;
        config.gpt.api_key = Some("sk-test".to_string());
        assert_claude_carried_search_refused(
            config,
            "gpt-test",
            (
                "codex-1-a@example.com-pro.json",
                r#"{"type":"codex","excluded_models":["gpt-5"]}"#,
            ),
            "Codex login codex-1-a@example.com-pro.json",
        )
        .await;
    }

    /// With `[muse]` off, `auto` relies on the child excluding every Meta
    /// model.
    #[tokio::test]
    async fn a_claude_carried_auto_search_is_refused_while_a_meta_login_lists_exclusions() {
        assert_claude_carried_search_refused(
            crate::config::Config::default(),
            "auto",
            (
                "meta-a@example.com.json",
                r#"{"type":"meta","excluded-models":"other"}"#,
            ),
            "Meta login meta-a@example.com.json",
        )
        .await;
    }
}
