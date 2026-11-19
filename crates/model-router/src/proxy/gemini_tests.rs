//! The Gemini family through the proxy, against a fake managed child:
//! `[gemini]` exists only on the managed upstream, so these cannot run
//! against the external one the integration tests use.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tower::ServiceExt;

use super::*;
use crate::config::{GeminiConfig, WebSearchConfig};

/// The path and body of each request a fake upstream got.
type Observed = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

/// A fake upstream at a loopback address, answering each request with
/// `handler`: its base URL and what it got.
async fn spawn_fake(
    handler: impl Fn(&axum::http::request::Parts, &Bytes) -> Response + Send + Sync + 'static,
) -> (String, Observed) {
    let observed: Observed = Arc::default();
    let seen = observed.clone();
    let handler = Arc::new(handler);
    let app = Router::new().fallback(any(move |request: Request| {
        let seen = seen.clone();
        let handler = handler.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = to_bytes(body, usize::MAX).await.unwrap();
            let document = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            seen.lock()
                .unwrap()
                .push((parts.uri.path().to_string(), document));
            handler(&parts, &body)
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), observed)
}

fn paths(observed: &Observed) -> Vec<String> {
    let observed = observed.lock().unwrap();
    observed.iter().map(|(path, _)| path.clone()).collect()
}

fn never_called(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
    panic!("nothing may reach this upstream, got {}", parts.uri.path());
}

fn gemini_config(allow_free_tier: bool) -> Config {
    Config {
        gemini: GeminiConfig {
            enabled: true,
            api_key: Some("AIza-test".to_string()),
            allow_free_tier,
        },
        ..Config::default()
    }
}

/// The gateway over a ready managed child at `child`, with `[gemini]`
/// enabled beside the built-in GPT routes and Anthropic at `anthropic`.
/// The free tier is allowed, so no billing probe comes between a test and
/// the child (the gate's tests use [`billing_app`]).
async fn gemini_app(child: &str, anthropic: &str, web_search: WebSearchMode) -> Router {
    let config = Config {
        anthropic_upstream_base: anthropic.to_string(),
        web_search: WebSearchConfig { mode: web_search },
        ..gemini_config(true)
    };
    let handle = crate::supervisor::ManagedHandle::ready_for_tests(child.to_string());
    build(config, Some(handle)).await.unwrap().0
}

async fn send(app: &Router, body: &serde_json::Value) -> (StatusCode, String) {
    let request = Request::post("/v1/messages")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn plain_message(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
    Response::new(Body::from(
        json!({
            "id": "msg_upstream", "type": "message", "role": "assistant",
            "model": "gemini-4-argon", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "pong"}],
            "usage": {"input_tokens": 10, "output_tokens": 1},
        })
        .to_string(),
    ))
}

/// A Gemini turn reaches the child under the built-in model ID with the
/// effort as its suffix (the child clamps it to the model's levels), behind
/// the identity block, and with the history as Claude Code sent it: the
/// child keeps Gemini's thought signatures in empty thinking blocks, which
/// a tool round trip must hand back untouched.
#[tokio::test]
async fn a_gemini_turn_carries_effort_as_a_suffix_and_its_history_untouched() {
    fn not_found(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        let mut response = Response::new(Body::from("no such model"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response
    }
    let (child, observed) = spawn_fake(plain_message).await;
    let (anthropic, anthropic_observed) = spawn_fake(not_found).await;
    let app = gemini_app(&child, &anthropic, WebSearchMode::Alpha).await;
    let messages = json!([
        {"role": "user", "content": "list the files"},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "", "signature": "cpa-gemini-carrier-v1:c2lnbmF0dXJl"},
            {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}},
        ]},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "a.txt"},
        ]},
        // Thinking that nothing answers: dropped for Muse, kept here.
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "", "signature": "cpa-gemini-carrier-v1:dHdv"},
        ]},
        {"role": "user", "content": "go on"},
    ]);
    for (model, display_name, effort, forwarded) in [
        (
            "gemini-4-argon",
            "Gemini 4 Argon",
            "xhigh",
            "gemini-4-argon(xhigh)",
        ),
        (
            "gemini-4-carbon",
            "Gemini 4 Carbon",
            "low",
            "gemini-4-carbon(low)",
        ),
        (
            "gemini-4-neon",
            "Gemini 4 Neon",
            "max",
            "gemini-4-neon(max)",
        ),
        (
            "gemini-4-hydrogen",
            "Gemini 4 Hydrogen",
            "xhigh",
            "gemini-4-hydrogen(xhigh)",
        ),
    ] {
        let (status, body) = send(
            &app,
            &json!({
                "model": model, "max_tokens": 32000,
                "thinking": {"type": "adaptive"},
                "output_config": {"effort": effort},
                "tool_choice": {"type": "tool", "name": "Bash"},
                "tools": [{"name": "Bash", "description": "Run a command",
                           "input_schema": {"type": "object"}}],
                "messages": messages,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (path, document) = observed.lock().unwrap().pop().unwrap();
        assert_eq!(path, "/v1/messages");
        assert_eq!(document["model"], forwarded);
        assert_eq!(document["messages"], messages);
        // A forced choice goes through: only Muse is limited to `auto`.
        assert_eq!(document["tool_choice"]["name"], "Bash");
        let identity = document["system"][0]["text"].as_str().unwrap();
        assert!(
            identity.starts_with(&format!("You are {display_name}, working inside")),
            "{identity}"
        );
    }
    // With the free tier allowed, nothing but the turns.
    assert_eq!(models(&observed), Vec::<String>::new());
    // A Gemini model the family does not route never reaches the child:
    // like any unrouted model, it goes to Anthropic, which has none such.
    assert_eq!(anthropic_observed.lock().unwrap().len(), 0);
    let (status, _) = send(
        &app,
        &json!({"model": "gemini-3.8-flash", "max_tokens": 16, "messages": []}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(observed.lock().unwrap().len(), 0);
    assert_eq!(anthropic_observed.lock().unwrap().len(), 1);
}

const SESSION_METADATA: &str =
    "{\"device_id\":\"d\",\"account_uuid\":\"a\",\"session_id\":\"sess-1\"}";

/// A main-loop turn on `model` whose reply calls `WebSearch`, so the tap
/// records the origin, then the sub-call Claude Code issues on `carrier`.
/// Returns the sub-call's body.
async fn turn_then_subcall(app: &Router, model: &str, carrier: &str) -> String {
    turn_calling_websearch(app, model).await;
    let (status, body) = send(app, &subcall(carrier)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// A main-loop turn on `model` whose reply calls `WebSearch`, read as far
/// as the call, so the tap has recorded the origin.
async fn turn_calling_websearch(app: &Router, model: &str) {
    let turn = json!({
        "model": model, "max_tokens": 8192, "stream": true,
        "metadata": {"user_id": SESSION_METADATA},
        "messages": [{"role": "user", "content": "find bun release notes"}],
        "tools": [{"name": "WebSearch", "description": "Search the web",
                   "input_schema": {"type": "object"}}],
    });
    let request = Request::post("/v1/messages")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(turn.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let mut seen = String::new();
    while !seen.contains("content_block_stop") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("the turn's stream stalled")
            .expect("the turn's stream ended before the tool call")
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}

/// The `WebSearch` sub-call, as Claude Code sends it on `carrier`.
fn subcall(carrier: &str) -> serde_json::Value {
    json!({
        "model": carrier, "max_tokens": 32000, "stream": true,
        "output_config": {"effort": "medium"},
        "metadata": {"user_id": SESSION_METADATA},
        "messages": [{"role": "user", "content": [{"type": "text",
            "text": "Perform a web search for the query: bun release notes"}]}],
        "system": [{"type": "text",
            "text": "You are an assistant for performing a web search tool use"}],
        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
        "tool_choice": {"type": "auto"},
    })
}

/// The child for a search test: a turn that calls `WebSearch`, and Codex's
/// search for the origins that have one.
fn searching_child(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
    match parts.uri.path() {
        "/v1/messages" => {
            let events = [
                json!({"type":"content_block_start","index":0,
                    "content_block":{"type":"tool_use","id":"toolu_1","name":"WebSearch","input":{}}}),
                json!({"type":"content_block_delta","index":0,
                    "delta":{"type":"input_json_delta",
                             "partial_json":"{\"query\":\"bun release notes\"}"}}),
                json!({"type":"content_block_stop","index":0}),
            ]
            .map(|data| Ok::<_, Infallible>(Bytes::from(format!("event: x\ndata: {data}\n\n"))));
            let mut response = Response::new(Body::from_stream(futures_util::stream::iter(events)));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            response
        }
        "/v1/alpha/search" => Response::new(Body::from(
            json!({
                "output": "Bun releases (https://bun.sh/blog)\nsnippets",
                "results": [{"type": "text_result", "title": "Bun releases",
                             "url": "https://bun.sh/blog", "domain": "bun.sh"}],
            })
            .to_string(),
        )),
        path if path.starts_with("/v1beta/models/") => Response::new(Body::from(
            json!({"candidates": [{
                "content": {"parts": [{"text": "Bun's releases are on its blog."}], "role": "model"},
                "groundingMetadata": {"groundingChunks": [
                    {"web": {"uri": "https://bun.sh/blog", "title": "bun.sh"}},
                ]},
            }]})
            .to_string(),
        )),
        path => panic!("a search must not reach {path}"),
    }
}

/// A Gemini agent's search is Google Search grounding on Hydrogen, through
/// the child, whatever carries the sub-call and whatever `[web-search]
/// mode` says: never Codex's search or Anthropic.
#[tokio::test]
async fn a_gemini_origin_websearch_is_answered_by_google_search_grounding() {
    for mode in [
        WebSearchMode::Alpha,
        WebSearchMode::Scrape,
        WebSearchMode::Off,
    ] {
        for carrier in ["claude-haiku-4-5", "gemini-4-carbon", "gpt-6-luna"] {
            let context = format!("{mode:?} on {carrier}");
            let (child, observed) = spawn_fake(searching_child).await;
            let (anthropic, anthropic_observed) = spawn_fake(never_called).await;
            let app = gemini_app(&child, &anthropic, mode).await;
            let body = turn_then_subcall(&app, "gemini-4-argon", carrier).await;
            assert!(body.contains("https://bun.sh/blog"), "{context}: {body}");
            assert!(
                !body.contains("web_search_tool_result_error"),
                "{context}: {body}"
            );
            assert_eq!(
                paths(&observed),
                [
                    "/v1/messages",
                    "/v1beta/models/gemini-4-hydrogen:generateContent"
                ],
                "{context}"
            );
            let (_, search) = observed.lock().unwrap()[1].clone();
            assert_eq!(search["tools"], json!([{"google_search": {}}]), "{context}");
            assert_eq!(anthropic_observed.lock().unwrap().len(), 0, "{context}");
        }
        // With no turn to tie it to, a sub-call on a Gemini carrier is the
        // carrier's own.
        let (child, observed) = spawn_fake(searching_child).await;
        let (anthropic, _) = spawn_fake(never_called).await;
        let app = gemini_app(&child, &anthropic, mode).await;
        let (status, body) = send(&app, &subcall("gemini-4-carbon")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("https://bun.sh/blog"), "{body}");
        assert_eq!(
            paths(&observed),
            ["/v1beta/models/gemini-4-hydrogen:generateContent"]
        );
    }
}

/// An answer the model gave without searching is a failed search, and says
/// so, rather than an empty one.
#[tokio::test]
async fn a_grounding_answer_without_sources_is_a_failed_search() {
    fn ungrounded(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        if parts.uri.path().starts_with("/v1beta/") {
            return Response::new(Body::from(
                json!({"candidates": [{"content": {"parts": [{"text": "I know this."}]}}]})
                    .to_string(),
            ));
        }
        searching_child(parts, body)
    }
    let (child, _) = spawn_fake(ungrounded).await;
    let (anthropic, _) = spawn_fake(never_called).await;
    let app = gemini_app(&child, &anthropic, WebSearchMode::Alpha).await;
    let body = turn_then_subcall(&app, "gemini-4-argon", "claude-haiku-4-5").await;
    assert!(body.contains("web_search_tool_result_error"), "{body}");
    assert!(
        body.contains("Google search returned no sources (the model answered without searching)"),
        "{body}"
    );
}

/// The origin decides, not the carrier: a GPT agent's search carried by a
/// Gemini route still goes to Codex's search.
#[tokio::test]
async fn a_gpt_origin_on_a_gemini_carrier_still_uses_alpha_search() {
    let (child, observed) = spawn_fake(searching_child).await;
    let (anthropic, anthropic_observed) = spawn_fake(never_called).await;
    let app = gemini_app(&child, &anthropic, WebSearchMode::Alpha).await;
    let body = turn_then_subcall(&app, "gpt-6-luna", "gemini-4-argon").await;
    assert!(body.contains("https://bun.sh/blog"), "{body}");
    assert!(!body.contains("web_search_tool_result_error"), "{body}");
    assert_eq!(paths(&observed), ["/v1/messages", "/v1/alpha/search"]);
    assert_eq!(anthropic_observed.lock().unwrap().len(), 0);
}

use crate::overflow::tests::CAPTURED_GEMINI_BODY as GEMINI_OVERFLOW_BODY;

/// Google's over-window error becomes Claude Code's on a Gemini route, so
/// the session compacts and retries; on any other route the same text is
/// not that route's dialect and passes through.
#[tokio::test]
async fn overflow_is_translated_only_on_gemini_routes() {
    fn overflowing(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        let mut response = Response::new(Body::from(GEMINI_OVERFLOW_BODY));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        response
    }
    let (child, _) = spawn_fake(overflowing).await;
    let (anthropic, _) = spawn_fake(never_called).await;
    let app = gemini_app(&child, &anthropic, WebSearchMode::Alpha).await;
    let request = |model: &str, stream: bool| {
        json!({
            "model": model, "max_tokens": 16, "stream": stream,
            "messages": [{"role": "user", "content": "hello"}],
        })
    };
    for model in ["gemini-4-argon", "gemini-4-carbon"] {
        for stream in [false, true] {
            let (status, body) = send(&app, &request(model, stream)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                envelope["error"]["message"],
                "prompt is too long: 1048577 tokens > 1048576 maximum",
                "{model} stream={stream}"
            );
        }
    }
    let (status, body) = send(&app, &request("gpt-6-luna", false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, GEMINI_OVERFLOW_BODY);
}

/// A streamed Gemini reply reaches the client with its text in one block,
/// the last one ([`crate::text_blocks`]); the same stream on another
/// family's route is not touched.
#[tokio::test]
async fn a_streamed_gemini_reply_has_its_text_in_one_last_block() {
    use crate::text_blocks::tests::{RECORDED_TEXT_REPLY, blocks};
    fn recorded(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        // Split mid-event, as a network would.
        let (head, tail) = RECORDED_TEXT_REPLY.split_at(RECORDED_TEXT_REPLY.len() / 2);
        let chunks = [head, tail].map(|chunk| Ok::<_, Infallible>(Bytes::from(chunk)));
        let mut response = Response::new(Body::from_stream(futures_util::stream::iter(chunks)));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        response
    }
    let (child, _) = spawn_fake(recorded).await;
    let (anthropic, _) = spawn_fake(never_called).await;
    let app = gemini_app(&child, &anthropic, WebSearchMode::Alpha).await;
    let request = |model: &str| {
        json!({
            "model": model, "max_tokens": 16, "stream": true,
            "messages": [{"role": "user", "content": "what are tide pools?"}],
        })
    };
    for model in ["gemini-4-argon", "gemini-4-carbon"] {
        let (status, body) = send(&app, &request(model)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let content = blocks(&body);
        assert_eq!(content.len(), 1, "{model}: {body}");
        assert_eq!(content[0]["type"], "text");
        let text = content[0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("Tide pools are rocky basins") && text.ends_with("mini-ecosystems."),
            "{model}: {text}"
        );
    }
    let (status, body) = send(&app, &request("gpt-6-luna")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, RECORDED_TEXT_REPLY);
}

/// The gateway over a ready managed child at `child` with `[gemini]` on,
/// and the gate Hydrogen requests pass ([`crate::gemini_billing`]).
async fn billing_app(
    child: &str,
    allow_free_tier: bool,
) -> (Router, Arc<crate::gemini_billing::Gate>) {
    gated_app(child, gemini_config(allow_free_tier)).await
}

/// The same, on `config`.
async fn gated_app(child: &str, config: Config) -> (Router, Arc<crate::gemini_billing::Gate>) {
    let handle = crate::supervisor::ManagedHandle::ready_for_tests(child.to_string());
    let state = AppState::new(config, Some(handle)).await.unwrap();
    let gate = state.gemini_billing.clone();
    (
        Router::new().fallback(any(dispatch)).with_state(state),
        gate,
    )
}

/// The models the billing probe asks, as the child gets them: Neon, and
/// Argon where Neon is rate-limited.
const PROBE: &str = "gemini-4-neon(low)";
const SECOND_PROBE: &str = "gemini-4-argon(low)";

/// What the child answers when Google refuses a paid-only model to a
/// project without billing: Google's status and its message, in an
/// Anthropic error.
const NO_BILLING: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"You exceeded your current quota, please check your plan and billing details. * Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-4-neon"}}"#;

/// What it answers for a billed project past a rate limit, in the shape of
/// Google's quota errors (the one seen live ended "Please retry in 5.2s").
const RATE_LIMITED: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"You exceeded your current quota, please check your plan and billing details. * Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_paid_tier_input_token_count, limit: 2000000\nPlease retry in 5.2s."}}"#;

fn answer(status: StatusCode, content_type: &'static str, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response
}

fn model_of(body: &Bytes) -> String {
    let document: serde_json::Value = serde_json::from_slice(body).unwrap();
    document["model"].as_str().unwrap().to_string()
}

/// A child on a key whose project has billing or not, as `billed` says at
/// the time: Hydrogen always answers, a paid-only model only while billed.
fn child_on_a_key(
    billed: Arc<AtomicBool>,
) -> impl Fn(&axum::http::request::Parts, &Bytes) -> Response + Send + Sync + 'static {
    move |parts, body| {
        if model_of(body).starts_with("gemini-4-hydrogen") || billed.load(Ordering::SeqCst) {
            plain_message(parts, body)
        } else {
            answer(
                StatusCode::TOO_MANY_REQUESTS,
                "application/json",
                NO_BILLING,
            )
        }
    }
}

/// The model of each request the child got, in order.
fn models(observed: &Observed) -> Vec<String> {
    let observed = observed.lock().unwrap();
    observed
        .iter()
        .map(|(_, document)| document["model"].as_str().unwrap().to_string())
        .collect()
}

fn turn(model: &str, content: &str, stream: bool) -> serde_json::Value {
    json!({
        "model": model, "max_tokens": 16, "stream": stream,
        "messages": [{"role": "user", "content": content}],
    })
}

async fn assert_served(app: &Router, model: &str) {
    let (status, body) = send(app, &turn(model, "hello", false)).await;
    assert_eq!(status, StatusCode::OK, "{model}: {body}");
}

async fn assert_refused_for_no_billing(app: &Router) {
    let (status, body) = send(app, &turn("gemini-4-hydrogen", "hello", true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(error["error"]["type"], "invalid_request_error");
    let message = error["error"]["message"].as_str().unwrap();
    for part in [
        "model-router refused this Gemini 4 Hydrogen request",
        "the key's project has no billing",
        "on its free tier, where it may use your prompts to improve its products",
        "https://aistudio.google.com/billing",
        "run /model-router:setup",
    ] {
        assert!(message.contains(part), "{part}: {message}");
    }
}

/// With the free tier not allowed, a Hydrogen request goes out only once a
/// probe found billing, which the gate runs on the first one; that answers
/// for the router's run. Probes are all it goes by: what the other routes
/// answer meanwhile, a reply or Google's free-tier error, changes nothing.
#[tokio::test]
async fn hydrogen_goes_out_once_a_probe_found_billing() {
    let billed = Arc::new(AtomicBool::new(true));
    let (child, observed) = spawn_fake(child_on_a_key(billed.clone())).await;
    let (app, _) = billing_app(&child, false).await;
    // A fresh start: one probe, then the request, and none for the next.
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_served(&app, "gemini-4-carbon").await;
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_eq!(
        models(&observed),
        [
            PROBE,
            "gemini-4-hydrogen",
            "gemini-4-carbon",
            "gemini-4-hydrogen"
        ]
    );
    let (path, probe) = observed.lock().unwrap()[0].clone();
    assert_eq!(path, "/v1/messages");
    assert_eq!(probe["stream"], serde_json::Value::Null);
    // What is left: billing unlinked while the router runs. Hydrogen goes
    // out until the router restarts, whatever Google tells another route.
    billed.store(false, Ordering::SeqCst);
    let (status, body) = send(&app, &turn("gemini-4-argon", "hello", true)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, NO_BILLING);
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_eq!(
        models(&observed)[4..],
        ["gemini-4-argon", "gemini-4-hydrogen"]
    );
}

/// A key without billing: the probe is answered from the free tier's quota,
/// so Hydrogen is refused and told why, and nothing of the request reaches
/// Google. The refusal answers for ten seconds, and the next probe finds
/// billing once it is on.
#[tokio::test]
async fn hydrogen_is_refused_while_the_key_has_no_billing() {
    let billed = Arc::new(AtomicBool::new(false));
    let (child, observed) = spawn_fake(child_on_a_key(billed.clone())).await;
    let (app, gate) = billing_app(&child, false).await;
    // One probe answers for the retry too.
    assert_refused_for_no_billing(&app).await;
    assert_refused_for_no_billing(&app).await;
    assert_eq!(models(&observed), [PROBE]);

    billed.store(true, Ordering::SeqCst);
    assert_refused_for_no_billing(&app).await;
    gate.expire();
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_eq!(models(&observed)[1..], [PROBE, "gemini-4-hydrogen"]);
}

/// A Hydrogen agent's search names its model only in the path, and passes
/// the same gate: on a key without billing it is a failed search that tells
/// why, and no grounding request goes out.
#[tokio::test]
async fn a_hydrogen_search_passes_the_billing_gate() {
    let on_a_key = child_on_a_key(Arc::new(AtomicBool::new(false)));
    let (child, observed) = spawn_fake(move |parts, body| {
        assert!(
            !parts.uri.path().starts_with("/v1beta/"),
            "a search went out without billing"
        );
        on_a_key(parts, body)
    })
    .await;
    let (app, _) = billing_app(&child, false).await;
    let (status, body) = send(&app, &subcall("gemini-4-hydrogen")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("web_search_tool_result_error"), "{body}");
    assert!(body.contains("the key's project has no billing"), "{body}");
    assert_eq!(models(&observed), [PROBE]);
}

/// `allow-free-tier = true` is the user's consent: Hydrogen goes out with
/// no probe, whatever the key's billing.
#[tokio::test]
async fn consent_to_the_free_tier_sends_hydrogen_with_no_probe() {
    let (child, observed) = spawn_fake(child_on_a_key(Arc::new(AtomicBool::new(false)))).await;
    let (app, _) = billing_app(&child, true).await;
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_eq!(models(&observed), ["gemini-4-hydrogen"]);
}

/// Hydrogen requests that arrive together wait on one probe and share its
/// answer, either way.
#[tokio::test]
async fn hydrogen_requests_that_arrive_together_share_one_probe() {
    /// The probe's answer, its body a while after its status.
    fn slow_probe(
        billed: bool,
    ) -> impl Fn(&axum::http::request::Parts, &Bytes) -> Response + Send + Sync + 'static {
        move |parts, body| {
            if model_of(body) != PROBE {
                return plain_message(parts, body);
            }
            let (status, body) = if billed {
                (StatusCode::OK, r#"{"type":"message"}"#)
            } else {
                (StatusCode::TOO_MANY_REQUESTS, NO_BILLING)
            };
            let late = futures_util::stream::once(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok::<_, Infallible>(Bytes::from(body))
            });
            answer(status, "application/json", Body::from_stream(late))
        }
    }
    for billed in [true, false] {
        let (child, observed) = spawn_fake(slow_probe(billed)).await;
        let (app, _) = billing_app(&child, false).await;
        let request = turn("gemini-4-hydrogen", "hello", false);
        let replies = futures_util::future::join_all((0..5).map(|_| send(&app, &request))).await;
        let expected = if billed {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        };
        for (status, body) in replies {
            assert_eq!(status, expected, "billed={billed}: {body}");
        }
        let models = models(&observed);
        assert_eq!(models[0], PROBE);
        assert_eq!(
            models[1..],
            vec!["gemini-4-hydrogen"; if billed { 5 } else { 0 }],
            "billed={billed}"
        );
    }
}

/// A Hydrogen request the gate refused with an error Claude Code retries:
/// its message.
async fn refused_for_now(app: &Router) -> String {
    let (status, body) = send(app, &turn("gemini-4-hydrogen", "hello", false)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let error: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(error["error"]["type"], "api_error");
    error["error"]["message"].as_str().unwrap().to_string()
}

/// A probe with no answer yet refuses Hydrogen with an error Claude Code
/// retries, as does a child that is not there to ask; one that a retry
/// cannot change, an invalid key's, with a plain 400.
#[tokio::test]
async fn hydrogen_is_refused_while_billing_cannot_be_checked() {
    fn overloaded(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        answer(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            r#"{"type":"error","error":{"type":"api_error","message":"The model is overloaded."}}"#,
        )
    }
    fn invalid_key(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        answer(
            StatusCode::BAD_REQUEST,
            "application/json",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"API key not valid. Please pass a valid API key."}}"#,
        )
    }
    let (child, observed) = spawn_fake(overloaded).await;
    let (app, _) = billing_app(&child, false).await;
    let message = refused_for_now(&app).await;
    for part in [
        "the key's billing could not be confirmed",
        "503 Service Unavailable: The model is overloaded.",
        "run /model-router:setup",
    ] {
        assert!(message.contains(part), "{part}: {message}");
    }
    assert_eq!(models(&observed), [PROBE]);

    let app = build(gemini_config(false), None).await.unwrap().0;
    let message = refused_for_now(&app).await;
    assert!(message.contains("supervisor failed to start"), "{message}");

    let (child, _) = spawn_fake(invalid_key).await;
    let (app, _) = billing_app(&child, false).await;
    let (status, body) = send(&app, &turn("gemini-4-hydrogen", "hello", false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("API key not valid"), "{body}");
}

/// A rate limit on Neon says nothing of billing (Google's limits are per
/// model), so the probe asks Argon once: its answer is the finding. With
/// both limited, the refusal is one Claude Code retries, and does not offer
/// the free tier as the way on.
#[tokio::test]
async fn a_rate_limited_probe_asks_a_second_paid_only_model() {
    /// A child whose Neon is past its rate limit, and whose Argon answers
    /// the probe with `argon`, or with a message for `None`.
    fn child(
        argon: Option<&'static str>,
    ) -> impl Fn(&axum::http::request::Parts, &Bytes) -> Response + Send + Sync + 'static {
        move |parts, body| match (model_of(body).as_str(), argon) {
            (PROBE, _) => answer(
                StatusCode::TOO_MANY_REQUESTS,
                "application/json",
                RATE_LIMITED,
            ),
            (SECOND_PROBE, Some(error)) => {
                answer(StatusCode::TOO_MANY_REQUESTS, "application/json", error)
            }
            _ => plain_message(parts, body),
        }
    }
    let (billed, observed) = spawn_fake(child(None)).await;
    let (app, _) = billing_app(&billed, false).await;
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_served(&app, "gemini-4-hydrogen").await;
    assert_eq!(
        models(&observed),
        [
            PROBE,
            SECOND_PROBE,
            "gemini-4-hydrogen",
            "gemini-4-hydrogen"
        ]
    );

    let (unbilled, observed) = spawn_fake(child(Some(NO_BILLING))).await;
    let (app, _) = billing_app(&unbilled, false).await;
    assert_refused_for_no_billing(&app).await;
    assert_eq!(models(&observed), [PROBE, SECOND_PROBE]);

    let (limited, observed) = spawn_fake(child(Some(RATE_LIMITED))).await;
    let (app, _) = billing_app(&limited, false).await;
    let message = refused_for_now(&app).await;
    for part in [
        "model-router refused this Gemini 4 Hydrogen request",
        "the key's billing could not be confirmed",
        "Google rate-limited the check on gemini-4-neon and on gemini-4-argon",
        "Please retry in 5.2s.",
    ] {
        assert!(message.contains(part), "{part}: {message}");
    }
    assert!(!message.contains("free tier"), "{message}");
    assert!(!message.contains("/model-router:setup"), "{message}");
    assert_eq!(models(&observed), [PROBE, SECOND_PROBE]);
}

/// The gate stands where a request leaves for the child, not where it
/// arrived. A GPT agent's search carried by a Hydrogen route asks Codex's
/// search first and Hydrogen only when that fails: the probe runs then, and
/// when it finds no billing, nothing of the sub-call reaches Hydrogen.
#[tokio::test]
async fn a_request_for_hydrogen_passes_the_gate_as_it_leaves() {
    for still_billed in [true, false] {
        let billed = Arc::new(AtomicBool::new(true));
        let codex_search = Arc::new(tokio::sync::Semaphore::new(0));
        let (on_a_key, held) = (child_on_a_key(billed.clone()), codex_search.clone());
        let (child, observed) = spawn_fake(move |parts, body| {
            if parts.uri.path() == "/v1/alpha/search" {
                // Codex's search, with nothing found once the test lets it
                // answer.
                let held = held.clone();
                let late = futures_util::stream::once(async move {
                    held.acquire().await.unwrap().forget();
                    Ok::<_, Infallible>(Bytes::from("{}"))
                });
                answer(StatusCode::OK, "application/json", Body::from_stream(late))
            } else if model_of(body).starts_with("gpt-") {
                searching_child(parts, body)
            } else {
                on_a_key(parts, body)
            }
        })
        .await;
        let (anthropic, _) = spawn_fake(never_called).await;
        let config = Config {
            anthropic_upstream_base: anthropic,
            web_search: WebSearchConfig {
                mode: WebSearchMode::Alpha,
            },
            ..gemini_config(false)
        };
        let (app, _) = gated_app(&child, config).await;
        turn_calling_websearch(&app, "gpt-6-luna").await;
        let sub_call = tokio::spawn({
            let app = app.clone();
            async move { send(&app, &subcall("gemini-4-hydrogen")).await }
        });
        let searching = || paths(&observed).last().map(String::as_str) == Some("/v1/alpha/search");
        for _ in 0..500 {
            if searching() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(searching(), "{:?}", paths(&observed));
        billed.store(still_billed, Ordering::SeqCst);
        codex_search.add_permits(1);
        let (status, body) = sub_call.await.unwrap();
        let since = models(&observed).split_off(2);
        if still_billed {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(since, [PROBE, "gemini-4-hydrogen(medium)"]);
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.contains("the key's project has no billing"), "{body}");
            assert_eq!(since, [PROBE]);
        }
    }
}
