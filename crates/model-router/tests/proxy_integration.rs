//! End-to-end tests of the gateway: each runs the real router (`proxy::app`
//! or `serve_listener`) against in-process fakes standing in for Anthropic
//! and `CLIProxyAPI`, and asserts on what the fakes saw and the client got.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Router, routing::any};
use futures_util::StreamExt;
use model_router::config::{
    CaptureConfig, Config, ModelRoute, NativeSearchLimits, UpstreamConfig, UpstreamMode,
};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;

#[derive(Clone)]
struct FakeState {
    observed: Arc<Mutex<Option<ObservedRequest>>>,
    release_second_chunk: Arc<Notify>,
}

#[derive(Debug)]
struct ObservedRequest {
    method: String,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

#[tokio::test]
async fn claude_body_is_exact_and_sse_is_streamed() {
    let fake_state = FakeState {
        observed: Arc::new(Mutex::new(None)),
        release_second_chunk: Arc::new(Notify::new()),
    };
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .fallback(any(fake_upstream))
        .with_state(fake_state.clone());
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let router_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router_address = router_listener.local_addr().unwrap();
    let config = Config {
        anthropic_upstream_base: format!("http://{fake_address}"),
        upstreams: external_upstreams(format!("http://{fake_address}")),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    };
    let router_task = tokio::spawn(async move {
        model_router::proxy::serve_listener(router_listener, config, None, std::future::pending())
            .await
            .unwrap();
    });

    let original = br#"{ "model" : "claude-sonnet-4-5", "messages" : [{"role":"user","content":"exact bytes"}], "stream":true }"#;
    let response = reqwest::Client::new()
        .post(format!(
            "http://{router_address}/v1/messages?beta=true&raw=%2F"
        ))
        .header("authorization", "Bearer exact-secret")
        .header("anthropic-beta", "oauth-feature")
        .body(original.as_slice())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("first SSE chunk should arrive")
        .expect("stream should contain first chunk")
        .unwrap();
    assert_eq!(first, "event: ping\ndata: first\n\n");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err(),
        "router must not buffer the full upstream response"
    );
    fake_state.release_second_chunk.notify_one();
    let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("second SSE chunk should arrive")
        .expect("stream should contain second chunk")
        .unwrap();
    assert_eq!(second, "event: ping\ndata: second\n\n");
    assert!(stream.next().await.is_none());

    let observed = fake_state.observed.lock().await.take().unwrap();
    assert_eq!(observed.method, "POST");
    assert_eq!(observed.uri, "/v1/messages?beta=true&raw=%2F");
    assert_eq!(observed.body, original.as_slice());
    assert_eq!(observed.headers["authorization"], "Bearer exact-secret");
    assert_eq!(observed.headers["anthropic-beta"], "oauth-feature");

    let gpt_response = reqwest::Client::new()
        .post(format!("http://{router_address}/v1/messages"))
        .header("authorization", "Bearer claude-secret")
        .header("x-api-key", "claude-api-key")
        .header("anthropic-beta", "oauth-feature")
        .header("content-type", "application/json")
        .body(r#"{"model":"claude-gpt-test","messages":[]}"#)
        .send()
        .await
        .unwrap();
    fake_state.release_second_chunk.notify_one();
    assert_eq!(gpt_response.status(), StatusCode::OK);
    gpt_response.bytes().await.unwrap();

    let observed = fake_state.observed.lock().await.take().unwrap();
    let document: serde_json::Value = serde_json::from_slice(&observed.body).unwrap();
    assert_eq!(document["model"], "gpt-test");
    assert!(
        document["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("GPT Test")
    );
    assert_eq!(observed.headers["authorization"], "Bearer gateway-secret");
    assert_eq!(observed.headers["x-api-key"], "gateway-secret");
    assert!(!observed.headers.contains_key("anthropic-beta"));

    router_task.abort();
    fake_task.abort();
}

#[tokio::test]
async fn stub_stream_and_capture_are_valid_and_redacted() {
    let directory = tempfile::tempdir().unwrap();
    let capture_file = directory.path().join("capture.jsonl");
    let config = Config {
        upstreams: stub_upstreams(),
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
            ..CaptureConfig::default()
        },
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages?beta=true")
        .header("authorization", "Bearer never-capture-this")
        .header("x-api-key", "also-never-capture-this")
        .header("cookie", "session=never-capture-this")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-gpt-test","stream":true,"messages":[]}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    for event in [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "message_delta",
        "message_stop",
    ] {
        assert!(body.contains(&format!("event: {event}")));
    }
    assert!(body.contains(r#""stop_reason":"end_turn""#));

    let capture = tokio::fs::read_to_string(capture_file).await.unwrap();
    assert_eq!(capture.lines().count(), 1);
    assert!(capture.contains(r#""branch":"gpt""#));
    assert!(capture.contains(r#""family":"gpt""#));
    assert!(capture.contains("[REDACTED]"));
    assert!(capture.contains("message_stop"));
    assert!(!capture.contains("never-capture-this"));

    let count_request = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"claude-gpt-test","messages":[]}"#))
        .unwrap();
    let count_response = app.oneshot(count_request).await.unwrap();
    assert_eq!(count_response.status(), StatusCode::NOT_FOUND);
    let count_body = to_bytes(count_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&count_body).contains("not_found_error"));
}

fn stub_upstreams() -> std::collections::BTreeMap<String, UpstreamConfig> {
    std::collections::BTreeMap::from([(
        "cliproxy".to_string(),
        UpstreamConfig {
            mode: UpstreamMode::Stub,
            ..UpstreamConfig::default()
        },
    )])
}

fn external_upstreams(base_url: String) -> std::collections::BTreeMap<String, UpstreamConfig> {
    std::collections::BTreeMap::from([(
        "cliproxy".to_string(),
        UpstreamConfig {
            mode: UpstreamMode::External,
            base_url: Some(base_url),
            api_key: Some("gateway-secret".to_string()),
            ..UpstreamConfig::default()
        },
    )])
}

async fn fake_upstream(State(state): State<FakeState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX).await.unwrap();
    *state.observed.lock().await = Some(ObservedRequest {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        headers: parts.headers,
        body,
    });

    let release = state.release_second_chunk.clone();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b"event: ping\ndata: first\n\n"));
        release.notified().await;
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b"event: ping\ndata: second\n\n"));
    };
    let mut response = Response::new(Body::from_stream(stream));
    response
        .headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    response
}

#[tokio::test]
async fn managed_unavailable_serves_claude_and_rejects_gpt() {
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new().fallback(any(|| async { "claude-upstream-ok" }));
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    // Managed mode with NO supervisor handle: the state after a failed
    // supervisor start. Claude traffic must pass; GPT must get a 502.
    let config = Config {
        anthropic_upstream_base: format!("http://{fake_address}"),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();

    let claude = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .body(Body::from(r#"{"model":"claude-sonnet-4-5","messages":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claude.status(), StatusCode::OK);
    let body = to_bytes(claude.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body, "claude-upstream-ok");

    let gpt = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .body(Body::from(r#"{"model":"claude-gpt-test","messages":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gpt.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(gpt.into_body(), usize::MAX).await.unwrap();
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Claude traffic is unaffected")
    );
    fake_task.abort();
}

#[tokio::test]
async fn ingress_token_gates_all_routes_except_bare_health() {
    let config = Config {
        upstreams: stub_upstreams(),
        ingress_token: Some("testtoken".to_string()),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let gpt_body = r#"{"model":"claude-gpt-test","messages":[]}"#;

    // Without the correct prefix: generic 404, never routed.
    for path in [
        "/v1/messages",
        "/t/wrongtoken/v1/messages",
        "/ttesttoken/v1/messages",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .body(Body::from(gpt_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path {path}");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            !String::from_utf8_lossy(&body).contains("testtoken"),
            "rejections must not leak the token"
        );
    }

    // With the prefix: routed to the stub normally.
    let routed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/t/testtoken/v1/messages")
                .body(Body::from(gpt_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(routed.status(), StatusCode::OK);

    // Health: reachable bare (for doctor) and under the prefix.
    for path in [
        "/__model-router/health",
        "/t/testtoken/__model-router/health",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "path {path}");
    }
}

#[tokio::test]
async fn gpt_sse_usage_rewrite_is_streamed_and_captured() {
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new().fallback(any(|| async {
        let chunks = [
            Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":"),
            Bytes::from_static(b"0,\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":42,\"cache_read_input_tokens\":5,\"output_tokens\":3}}\n\n"),
        ];
        let stream = futures_util::stream::iter(
            chunks.into_iter().map(Ok::<Bytes, std::convert::Infallible>),
        );
        let mut response = Response::new(Body::from_stream(stream));
        response
            .headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        response
    }));
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let directory = tempfile::tempdir().unwrap();
    let capture_file = directory.path().join("capture.jsonl");
    let config = Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
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
            ..CaptureConfig::default()
        },
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"claude-gpt-test","stream":true,"system":"harness","messages":[{"role":"user","content":"hello"}],"tools":[{"name":"Read","description":"Read a file","input_schema":{"type":"object"}}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let client_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let client_text = String::from_utf8(client_body.to_vec()).unwrap();
    assert!(client_text.contains("event: message_start"));
    assert!(!client_text.contains(r#""input_tokens":0"#));
    assert!(client_text.contains(r#""input_tokens":42"#));

    let jsonl = tokio::fs::read_to_string(capture_file).await.unwrap();
    let record: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
    assert_eq!(record["response_body"], client_text);
    fake_task.abort();
}

/// A route whose real window is four times what Claude Code believes reports a
/// quarter of its real usage, so the client's auto-compact gate — which sums
/// the usage of the last message carrying one — trips at the real limit.
/// Deprecated, but kept working for configs that still ask for it.
#[tokio::test]
async fn scaled_route_reports_usage_in_the_clients_coordinate_system() {
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new().fallback(any(|| async {
        let chunks = [
            Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n"),
            Bytes::from_static(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":400000,\"cache_read_input_tokens\":80000,\"output_tokens\":4000}}\n\n"),
        ];
        let stream = futures_util::stream::iter(
            chunks.into_iter().map(Ok::<Bytes, std::convert::Infallible>),
        );
        let mut response = Response::new(Body::from_stream(stream));
        response
            .headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        response
    }));
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let config = Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        declared_context_window: Some(250_000),
        models: vec![ModelRoute {
            routing_id: "kimi-k3".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "kimi-k3".to_string(),
            display_name: "Kimi K3".to_string(),
            family: model_router::config::ModelFamily::OpenAiCompat,
            context_window: Some(1_000_000),
            context_window_scaling: true,
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"kimi-k3","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    for expected in [
        r#""input_tokens":100000"#,
        r#""cache_read_input_tokens":20000"#,
        r#""output_tokens":1000"#,
    ] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
    fake_task.abort();
}

/// The overflow error body captured live from CLIProxyAPI 7.2.92
/// (2026-07-29): what the Codex backend's `context_length_exceeded` looks
/// like after CLIProxyAPI's translation drops the error code.
const CODEX_OVERFLOW_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your input exceeds the context window of this model. Please adjust your input and try again."}}"#;

/// A non-streaming overflow 400 from the Codex backend is rewritten into
/// Anthropic's canonical `prompt is too long` error — the shape Claude
/// Code's reactive compact-and-retry recovery is keyed on — with the
/// upstream's stale Content-Length dropped alongside the body change.
#[tokio::test]
async fn gpt_overflow_400_is_translated_to_the_canonical_anthropic_error() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        let mut response = Response::new(Body::from(CODEX_OVERFLOW_BODY));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        response
    }
    let (fake_address, observed) = spawn_fake(handler).await;

    // Default config: the built-in `gpt-5.6-sol` route is Codex-native with
    // the Codex 828400 maximum window.
    let config = Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("accept-encoding", "gzip")
                .body(Body::from(
                    r#"{"model":"gpt-5.6-sol","messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get("content-length").is_none());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["type"], "error");
    assert_eq!(envelope["error"]["type"], "invalid_request_error");
    let message = envelope["error"]["message"].as_str().unwrap();
    // The tiny request body estimates under the window, so N is the clamp.
    assert_eq!(
        message,
        "prompt is too long: 828401 tokens > 828400 maximum"
    );

    // The overflow translator parses raw response bytes, so the GPT branch
    // must request identity explicitly — an absent Accept-Encoding would
    // still permit the server to compress.
    let observed = observed.lock().await;
    assert_eq!(
        observed
            .last()
            .unwrap()
            .headers
            .get("accept-encoding")
            .map(|value| value.to_str().unwrap().to_string()),
        Some("identity".to_string())
    );
}

/// A streamed overflow fails as an in-stream `error` event on a 200
/// response (captured live); it is rewritten the same way, and the
/// surrounding events stream through.
#[tokio::test]
async fn gpt_overflow_sse_error_event_is_translated() {
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new().fallback(any(|| async {
        let chunks = [
            Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n"),
            Bytes::from(format!("event: error\ndata: {CODEX_OVERFLOW_BODY}\n\n")),
        ];
        let stream = futures_util::stream::iter(
            chunks.into_iter().map(Ok::<Bytes, std::convert::Infallible>),
        );
        let mut response = Response::new(Body::from_stream(stream));
        response
            .headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        response
    }));
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let config = Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.6-sol","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("event: message_start"), "{text}");
    assert!(
        text.contains("prompt is too long: 828401 tokens > 828400 maximum"),
        "{text}"
    );
    assert!(!text.contains("exceeds the context window"), "{text}");
    fake_task.abort();
}

/// Overflow translation is verified for the Codex backend alone; a route to
/// any other upstream model passes its errors through untouched even when
/// the message happens to match.
#[tokio::test]
async fn non_codex_route_overflow_passes_through_untouched() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        let mut response = Response::new(Body::from(CODEX_OVERFLOW_BODY));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        response
    }
    let (fake_address, _observed) = spawn_fake(handler).await;

    let config = Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        models: vec![ModelRoute {
            routing_id: "kimi-k3".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "kimi-k3".to_string(),
            display_name: "Kimi K3".to_string(),
            context_window: Some(250_000),
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"kimi-k3","messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body, Bytes::from(CODEX_OVERFLOW_BODY));
}

/// Spawns a single-purpose fake upstream returning `handler`'s response and
/// recording every request.
async fn spawn_fake(
    handler: fn(&axum::http::request::Parts, &Bytes) -> Response,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<ObservedRequest>>>) {
    #[derive(Clone)]
    struct HandlerState {
        observed: Arc<Mutex<Vec<ObservedRequest>>>,
        handler: fn(&axum::http::request::Parts, &Bytes) -> Response,
    }
    async fn serve(State(state): State<HandlerState>, request: Request) -> Response {
        let (parts, body) = request.into_parts();
        let body = to_bytes(body, usize::MAX).await.unwrap();
        state.observed.lock().await.push(ObservedRequest {
            method: parts.method.to_string(),
            uri: parts.uri.to_string(),
            headers: parts.headers.clone(),
            body: body.clone(),
        });
        (state.handler)(&parts, &body)
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let state = HandlerState {
        observed: observed.clone(),
        handler,
    };
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(serve)).with_state(state),
        )
        .await
        .unwrap();
    });
    (address, observed)
}

fn websearch_subcall_body() -> String {
    serde_json::json!({
        "model": "claude-gpt-test",
        "max_tokens": 4096,
        "stream": true,
        "messages": [{"role": "user",
            "content": "Perform a web search for the query: rust axum shutdown"}],
        "system": [{"type": "text",
            "text": "You are an assistant for performing a web search tool use"}],
        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
        "tool_choice": {"type": "tool", "name": "web_search"},
    })
    .to_string()
}

fn websearch_config(fake_address: std::net::SocketAddr) -> Config {
    Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    }
}

#[tokio::test]
async fn websearch_subcall_is_answered_from_alpha_search() {
    fn handler(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        assert_eq!(parts.uri.path(), "/v1/alpha/search");
        Response::new(Body::from(
            serde_json::json!({
                "encrypted_output": "opaque",
                "output": "Axum docs (https://docs.rs/axum)\n\u{E200}cite\u{E202}turn0search0\u{E201} [wordlim: 200] Graceful shutdown notes.",
                "results": [
                    {"type": "text_result", "title": "Axum docs", "url": "https://docs.rs/axum",
                     "domain": "docs.rs", "ref_id": "turn0search0",
                     "snippet": "Graceful shutdown"},
                    {"type": "text_result", "title": "Axum docs", "url": "https://docs.rs/axum"},
                ],
            })
            .to_string(),
        ))
    }
    let (fake_address, observed) = spawn_fake(handler).await;
    let app = model_router::proxy::app(websearch_config(fake_address))
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_subcall_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    for event in [
        "event: message_start",
        "event: content_block_start",
        "event: message_delta",
        "event: message_stop",
    ] {
        assert!(body.contains(event), "missing {event} in {body}");
    }
    assert!(body.contains(r#""type":"server_tool_use""#));
    assert!(body.contains(r#""type":"web_search_tool_result""#));
    assert!(body.contains("https://docs.rs/axum"));
    assert!(body.contains("Graceful shutdown notes."));
    assert!(
        !body.contains('\u{E200}'),
        "citation markers must be stripped"
    );
    assert!(body.contains(r#""stop_reason":"end_turn""#));

    let observed = observed.lock().await;
    assert_eq!(observed.len(), 1);
    let request = &observed[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.uri, "/v1/alpha/search");
    assert_eq!(request.headers["authorization"], "Bearer gateway-secret");
    let document: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    // The alpha backend is ChatGPT's Codex search endpoint, reached under the
    // Codex credential, so it is addressed with a Codex slug — not with the
    // requesting route's upstream model (here the non-Codex `gpt-test`).
    assert_eq!(document["model"], "gpt-6-astra");
    assert_eq!(
        document["commands"]["search_query"][0]["q"],
        "rust axum shutdown"
    );
}

#[tokio::test]
async fn websearch_falls_back_to_buffered_llm_call_with_scraped_links() {
    fn handler(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        if parts.uri.path() == "/v1/alpha/search" {
            let mut response = Response::new(Body::from("search backend down"));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return response;
        }
        assert_eq!(parts.uri.path(), "/v1/messages");
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            document["stream"], false,
            "fallback must buffer the response"
        );
        Response::new(Body::from(
            serde_json::json!({
                "id": "msg_upstream",
                "type": "message",
                "role": "assistant",
                "model": "gpt-test",
                "stop_reason": "end_turn",
                "content": [
                    {"type": "server_tool_use", "id": "srvtoolu_x", "name": "web_search",
                     "input": {"query": "rust axum shutdown"}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_x",
                     "content": []},
                    {"type": "text",
                     "text": "See ([docs.rs](https://docs.rs/axum?utm_source=openai))."},
                ],
                "usage": {"input_tokens": 0, "output_tokens": 50},
            })
            .to_string(),
        ))
    }
    let (fake_address, observed) = spawn_fake(handler).await;
    let app = model_router::proxy::app(websearch_config(fake_address))
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("x-claude-code-session-id", "session-1")
                .header("x-claude-code-agent-id", "agent-1")
                .body(Body::from(websearch_subcall_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains(r#""type":"web_search_tool_result""#));
    // The empty result block was filled from the text citation, with the
    // tracking parameter stripped.
    assert!(body.contains(r#""url":"https://docs.rs/axum""#));
    assert!(body.contains(r#""title":"docs.rs""#));
    // The zero input-token report was replaced by the estimate.
    assert!(!body.contains(r#""input_tokens":0"#));
    assert!(body.contains("event: message_stop"));

    let observed = observed.lock().await;
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].uri, "/v1/alpha/search");
    assert_eq!(observed[1].uri, "/v1/messages");
    assert_eq!(
        observed[1].headers["authorization"],
        "Bearer gateway-secret"
    );
    // The buffered fallback carries the shared-prefix cache identity too.
    assert!(
        observed[1].headers["x-claude-code-session-id"]
            .to_str()
            .unwrap()
            .starts_with("prefix-")
    );
    assert!(!observed[1].headers.contains_key("x-claude-code-agent-id"));
}

#[tokio::test]
async fn an_oversized_request_body_is_refused_with_413() {
    let config = Config {
        upstreams: stub_upstreams(),
        max_request_body_bytes: 64,
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = format!(
        r#"{{"model":"gpt-5.6-sol","messages":[{{"role":"user","content":"{}"}}]}}"#,
        "x".repeat(200)
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["type"], "invalid_request_error");
}

// ---- origin-matched backend routing ----

const SESSION_METADATA: &str =
    "{\"device_id\":\"d\",\"account_uuid\":\"a\",\"session_id\":\"sess-1\"}";

/// A main-loop-shaped request declaring the client WebSearch tool.
fn websearch_declaring_body(model: &str) -> String {
    serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "stream": true,
        "metadata": {"user_id": SESSION_METADATA},
        "messages": [{"role": "user", "content": "find bun release notes"}],
        "tools": [{"name": "WebSearch", "description": "Search the web",
                   "input_schema": {"type": "object"}},
                  {"name": "Bash", "description": "Run a command",
                   "input_schema": {"type": "object"}}],
    })
    .to_string()
}

/// The sub-call the harness issues after a WebSearch tool_use, on the main
/// model. Includes main-model tuning fields to exercise normalization.
fn origin_subcall_body(main_model: &str) -> String {
    serde_json::json!({
        "model": main_model,
        "max_tokens": 32000,
        "stream": true,
        "output_config": {"effort": "medium"},
        "metadata": {"user_id": SESSION_METADATA},
        "messages": [{"role": "user",
            "content": [{"type": "text",
                "text": "Perform a web search for the query: bun release notes"}]}],
        "system": [{"type": "text", "text": "You are an assistant for performing a web search tool use"}],
        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
        "tool_choice": {"type": "auto"},
    })
    .to_string()
}

/// SSE events for a response whose model calls the WebSearch tool.
fn websearch_tool_use_sse() -> Vec<Bytes> {
    [
        serde_json::json!({"type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"toolu_1","name":"WebSearch","input":{}}}),
        serde_json::json!({"type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta",
                     "partial_json":"{\"query\":\"bun release notes\"}"}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
    ]
    .iter()
    .map(|data| Bytes::from(format!("event: x\ndata: {data}\n\n")))
    .collect()
}

fn sse_response(chunks: Vec<Bytes>, hold_open: bool) -> Response {
    let stream = futures_util::stream::iter(
        chunks
            .into_iter()
            .map(Ok::<Bytes, std::convert::Infallible>),
    );
    let body = if hold_open {
        Body::from_stream(stream.chain(futures_util::stream::pending()))
    } else {
        Body::from_stream(stream)
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        "content-type",
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn alpha_results_response() -> Response {
    Response::new(Body::from(
        serde_json::json!({
            "output": "Bun releases (https://bun.sh/blog)\nsnippets",
            "results": [{"type": "text_result", "title": "Bun releases",
                         "url": "https://bun.sh/blog", "domain": "bun.sh"}],
        })
        .to_string(),
    ))
}

/// Reads the client-visible body stream until `marker` appears; panics after
/// too many chunks. Returns without consuming the rest of the stream.
async fn read_until(body: &mut axum::body::BodyDataStream, marker: &str) {
    let mut seen = String::new();
    for _ in 0..64 {
        let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("stream chunk should arrive")
            .expect("stream should not end before the marker")
            .expect("stream chunk should be Ok");
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.contains(marker) {
            return;
        }
    }
    panic!("marker {marker:?} not found in stream");
}

/// Claude main + GPT subagent: the WebSearch tool_use is observed on the GPT
/// branch, and the follow-up sub-call — arriving on the CLAUDE branch with
/// the main model — is answered from alpha/search. Structured as an ordering
/// race: the GPT response stream is held open and the sub-call is sent the
/// moment the client sees the completing event.
#[tokio::test]
async fn gpt_origin_subcall_on_claude_branch_is_answered_from_alpha() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/alpha/search" => alpha_results_response(),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();

    // GPT subagent turn: response streams the WebSearch tool_use, then stays
    // open (the model may keep generating).
    let gpt_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gpt_turn.status(), StatusCode::OK);
    let mut gpt_stream = gpt_turn.into_body().into_data_stream();
    read_until(&mut gpt_stream, "content_block_stop").await;

    // The harness reacts immediately: the sub-call arrives on the Claude
    // branch (main model) while the GPT stream is still open.
    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-sonnet-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains(r#""type":"web_search_tool_result""#));
    assert!(body.contains("https://bun.sh/blog"));

    assert_eq!(anthropic_observed.lock().await.len(), 0);
    let cpa_requests = cpa_observed.lock().await;
    assert_eq!(
        cpa_requests
            .iter()
            .map(|request| request.uri.as_str())
            .collect::<Vec<_>>(),
        ["/v1/messages", "/v1/alpha/search"]
    );
}

/// GPT main + Claude subagent: the WebSearch tool_use is observed on the
/// Claude branch, and the sub-call — arriving on the GPT branch with the
/// main (GPT) model — is forwarded to Anthropic as a normalized request for
/// the origin Claude model.
#[tokio::test]
async fn claude_origin_subcall_on_gpt_branch_goes_to_anthropic_native() {
    fn anthropic(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        assert_eq!(parts.uri.path(), "/v1/messages");
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            // The Claude subagent's own turn: stream a WebSearch tool_use.
            return sse_response(websearch_tool_use_sse(), false);
        }
        // The redirected sub-call: assert normalization.
        assert_eq!(document["model"], "claude-haiku-4-5");
        assert_eq!(document["stream"], false);
        assert_eq!(document["max_tokens"], 8192, "max_tokens must be clamped");
        assert!(
            document.get("output_config").is_none(),
            "tuning fields must be dropped"
        );
        assert!(
            !body_contains(body, "GPT Test"),
            "identity block must not reach Anthropic"
        );
        assert_eq!(parts.headers["authorization"], "Bearer user-oauth");
        assert_eq!(parts.headers["anthropic-beta"], "oauth-2025-04-20");
        Response::new(Body::from(
            serde_json::json!({
                "id": "msg_native", "type": "message", "role": "assistant",
                "model": "claude-haiku-4-5", "stop_reason": "end_turn",
                "content": [
                    {"type": "server_tool_use", "id": "srvtoolu_n", "name": "web_search",
                     "input": {"query": "bun release notes"}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_n",
                     "content": [{"type": "web_search_result", "title": "Bun releases",
                                  "url": "https://bun.sh/blog"}]},
                ],
                "usage": {"input_tokens": 100, "output_tokens": 200},
            })
            .to_string(),
        ))
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!(
            "the GPT upstream must not be called, got {}",
            parts.uri.path()
        );
    }
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();

    // Claude subagent turn (Claude branch): emits the WebSearch tool_use.
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("authorization", "Bearer user-oauth")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    // The sub-call arrives on the GPT branch (main model is GPT).
    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("authorization", "Bearer user-oauth")
                .header("anthropic-beta", "oauth-2025-04-20")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    assert_eq!(subcall.headers()["content-type"], "text/event-stream");
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains(r#""type":"web_search_tool_result""#));
    assert!(body.contains("https://bun.sh/blog"));

    assert_eq!(cpa_observed.lock().await.len(), 0);
    assert_eq!(anthropic_observed.lock().await.len(), 2);
}

fn body_contains(body: &Bytes, needle: &str) -> bool {
    String::from_utf8_lossy(body).contains(needle)
}

/// Anthropic caller-error statuses on the native redirect are surfaced, not
/// hidden behind a silent provider switch.
#[tokio::test]
async fn claude_origin_auth_errors_are_surfaced_not_fallen_back() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        let mut response = Response::new(Body::from(
            r#"{"type":"error","error":{"type":"authentication_error","message":"bad token"}}"#,
        ));
        *response.status_mut() = StatusCode::UNAUTHORIZED;
        response
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!(
            "the GPT upstream must not be called, got {}",
            parts.uri.path()
        );
    }
    let (anthropic_address, _) = spawn_fake(anthropic).await;
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("authentication_error"));
    assert_eq!(cpa_observed.lock().await.len(), 0);
}

/// A transient Anthropic failure on the native redirect falls back to the
/// GPT path (alpha) — nothing has been streamed yet, so the switch is
/// lossless.
#[tokio::test]
async fn claude_origin_falls_back_to_gpt_path_on_transient_anthropic_failure() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        let mut response = Response::new(Body::from("overloaded"));
        *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        response
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/alpha/search" => alpha_results_response(),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("https://bun.sh/blog"));
    // The Claude turn, then the failed native redirect.
    assert_eq!(anthropic_observed.lock().await.len(), 2);
    assert_eq!(
        cpa_observed
            .lock()
            .await
            .iter()
            .map(|request| request.uri.as_str())
            .collect::<Vec<_>>(),
        ["/v1/alpha/search"]
    );
}

/// `off` mode forwards sub-calls on both branches without interception.
#[tokio::test]
async fn off_mode_forwards_subcalls_without_interception() {
    fn upstream(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        assert_eq!(parts.uri.path(), "/v1/messages");
        Response::new(Body::from(
            r#"{"id":"msg","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
        ))
    }
    let (address, observed) = spawn_fake(upstream).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{address}"),
        web_search: model_router::config::WebSearchConfig {
            mode: model_router::config::WebSearchMode::Off,
        },
        ..websearch_config(address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    // A GPT-branch sub-call is forwarded to the GPT upstream untouched
    // (no alpha call), and a Claude-branch sub-call passes through.
    for model in ["claude-gpt-test", "claude-sonnet-4-5"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(origin_subcall_body(model)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), usize::MAX).await.unwrap();
    }
    let observed = observed.lock().await;
    assert_eq!(observed.len(), 2);
    assert!(observed.iter().all(|request| request.uri == "/v1/messages"));
}

/// The Anthropic-native redirect needs no GPT upstream: it must work even
/// while the managed upstream is unavailable.
#[tokio::test]
async fn claude_origin_native_works_without_a_gpt_upstream() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        assert_eq!(document["model"], "claude-haiku-4-5");
        Response::new(Body::from(
            serde_json::json!({
                "id": "msg_native", "type": "message", "role": "assistant",
                "model": "claude-haiku-4-5", "stop_reason": "end_turn",
                "content": [{"type": "web_search_tool_result", "tool_use_id": "s",
                             "content": [{"type": "web_search_result", "title": "Bun",
                                          "url": "https://bun.sh/blog"}]}],
                "usage": {"input_tokens": 1, "output_tokens": 1},
            })
            .to_string(),
        ))
    }
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    // Default upstreams = managed mode; `app()` provides no managed handle,
    // so the GPT target is ManagedUnavailable.
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("https://bun.sh/blog"));
    assert_eq!(anthropic_observed.lock().await.len(), 2);
}

/// A complete 200 whose body is not an Anthropic message (e.g. an
/// intermediary's HTML error page) is a protocol defect, surfaced rather
/// than silently re-routed to the GPT backend.
#[tokio::test]
async fn claude_origin_malformed_200_is_surfaced() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        Response::new(Body::from("<html>intermediary error page</html>"))
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!(
            "the GPT upstream must not be called, got {}",
            parts.uri.path()
        );
    }
    let (anthropic_address, _) = spawn_fake(anthropic).await;
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("intermediary error page"));
    assert_eq!(cpa_observed.lock().await.len(), 0);
}

/// Non-transient statuses outside the happy path — not just auth errors —
/// are surfaced rather than hidden behind a provider switch.
#[tokio::test]
async fn claude_origin_unlisted_client_errors_are_surfaced() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        let mut response = Response::new(Body::from(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"unprocessable"}}"#,
        ));
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        response
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!(
            "the GPT upstream must not be called, got {}",
            parts.uri.path()
        );
    }
    let (anthropic_address, _) = spawn_fake(anthropic).await;
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let claude_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-haiku-4-5")))
                .unwrap(),
        )
        .await
        .unwrap();
    to_bytes(claude_turn.into_body(), usize::MAX).await.unwrap();

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("unprocessable"));
    assert_eq!(cpa_observed.lock().await.len(), 0);
}

#[tokio::test]
async fn gpt_branch_rewrites_cache_identity_headers_to_the_shared_prefix_key() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        Response::new(Body::from(
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"gpt-test","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
        ))
    }
    let (fake_address, observed) = spawn_fake(handler).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{fake_address}"),
        upstreams: external_upstreams(format!("http://{fake_address}")),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();

    for (session, system) in [
        ("session-a", r#""shared agent prompt""#),
        ("session-b", r#""shared agent prompt""#),
        ("session-c", r#""a different agent prompt""#),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .header("x-claude-code-session-id", session)
                    .header("x-claude-code-agent-id", format!("agent-of-{session}"))
                    .body(Body::from(format!(
                        r#"{{"model":"claude-gpt-test","system":{system},"messages":[]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let observed = observed.lock().await;
    let forwarded: Vec<_> = observed.iter().collect();
    assert_eq!(forwarded.len(), 3);
    // Different conversations, same system head: one upstream identity.
    let key = forwarded[0].headers["x-claude-code-session-id"].clone();
    assert!(key.to_str().unwrap().starts_with("prefix-"), "{key:?}");
    assert_eq!(forwarded[1].headers["x-claude-code-session-id"], key);
    for request in &forwarded {
        assert!(!request.headers.contains_key("x-claude-code-agent-id"));
    }
    // A different system head is a different identity.
    assert_ne!(forwarded[2].headers["x-claude-code-session-id"], key);
}

// ---- Grok-origin WebSearch: xAI-native search ----

/// A config with both a Grok route and a GPT route, so the gate is exercised
/// against a real choice rather than a single-family config.
fn grok_websearch_config(fake_address: std::net::SocketAddr) -> Config {
    Config {
        upstreams: external_upstreams(format!("http://{fake_address}")),
        models: vec![
            ModelRoute {
                routing_id: "grok-4.5".to_string(),
                upstream: "cliproxy".to_string(),
                upstream_model: "grok-4.5".to_string(),
                display_name: "Grok 4.5".to_string(),
                family: model_router::config::ModelFamily::Grok,
                ..Default::default()
            },
            ModelRoute {
                routing_id: "claude-gpt-test".to_string(),
                upstream: "cliproxy".to_string(),
                upstream_model: "gpt-test".to_string(),
                display_name: "GPT Test".to_string(),
                ..Default::default()
            },
        ],
        ..Config::default()
    }
}

/// The xAI Responses stream: a completed hosted `web_search_call` carrying
/// sources, then (optionally) an open-ended tail the router must not wait for.
fn xai_search_sse(urls: &[&str], hold_open: bool) -> Response {
    let sources = urls
        .iter()
        .map(|url| serde_json::json!({"type": "url", "url": url}))
        .collect::<Vec<_>>();
    let chunks = [
        serde_json::json!({"type": "response.created", "response": {"id": "r1"}}),
        serde_json::json!({"type": "response.output_item.done", "item": {
            "id": "ws_1", "type": "web_search_call", "status": "completed",
            "action": {"type": "search", "query": "bun release notes",
                       "sources": sources}}}),
    ]
    .iter()
    .map(|data| Bytes::from(format!("event: x\ndata: {data}\n\n")))
    .collect();
    sse_response(chunks, hold_open)
}

fn terminal_only_sse() -> Response {
    sse_response(
        [
            serde_json::json!({"type": "response.created", "response": {"id": "r1"}}),
            serde_json::json!({"type": "response.completed"}),
        ]
        .iter()
        .map(|data| Bytes::from(format!("event: x\ndata: {data}\n\n")))
        .collect(),
        false,
    )
}

/// Drives a main-loop turn on `turn_model` — whose response streams a
/// `WebSearch` tool_use, so the tap records that origin — and then the
/// follow-up sub-call carried by `subcall_model`. Returns the sub-call's body.
async fn turn_then_subcall(app: axum::Router, turn_model: &str, subcall_model: &str) -> String {
    let turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body(turn_model)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(turn.status(), StatusCode::OK);
    let mut stream = turn.into_body().into_data_stream();
    read_until(&mut stream, "content_block_stop").await;

    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(origin_subcall_body(subcall_model)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

fn observed_paths(observed: &[ObservedRequest]) -> Vec<&str> {
    observed
        .iter()
        .map(|request| request.uri.as_str())
        .collect()
}

/// A Grok agent's search is answered from xAI's hosted `web_search` —
/// never from alpha/search, never from Anthropic — and the router answers as
/// soon as the sources arrive, without waiting for the stream to end.
#[tokio::test]
async fn grok_origin_websearch_is_answered_from_xai_native_search() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                let document: serde_json::Value = serde_json::from_slice(body).unwrap();
                assert_eq!(document["model"], "grok-4.5");
                assert_eq!(document["input"], "bun release notes");
                assert_eq!(document["tools"][0]["type"], "web_search");
                assert_eq!(document["tool_choice"], "required");
                assert_eq!(document["stream"], true);
                assert_eq!(parts.headers["authorization"], "Bearer gateway-secret");
                // Held open: passing proves the router does not wait for the
                // synthesis tail.
                xai_search_sse(&["https://bun.sh/blog", "https://bun.sh/docs"], true)
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..grok_websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "grok-4.5", "claude-sonnet-4-5").await;

    assert!(body.contains(r#""type":"web_search_tool_result""#));
    assert!(body.contains("https://bun.sh/blog"));
    assert!(body.contains("https://bun.sh/docs"));
    // No titles exist on this path: the URL is its own label.
    assert!(body.contains(r#""title":"https://bun.sh/blog""#));
    assert!(!body.contains("web_search_tool_result_error"));

    assert_eq!(anthropic_observed.lock().await.len(), 0);
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/responses"],
        "the search must go to /v1/responses and nowhere else"
    );
}

/// An xAI search that errors is reported as a failed search. It must not
/// fall back to alpha/search, to the origin route, or to Anthropic.
#[tokio::test]
async fn grok_origin_websearch_failure_is_visible_and_never_falls_back() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                let mut response = Response::new(Body::from("upstream exploded"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..grok_websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "grok-4.5", "claude-sonnet-4-5").await;

    assert!(body.contains("web_search_tool_result_error"));
    assert!(body.contains(r#""error_code":"unavailable""#));
    // A failed search must not spend the session's WebSearch budget.
    assert!(!body.contains("web_search_requests"));

    assert_eq!(anthropic_observed.lock().await.len(), 0);
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/responses"],
        "no alpha/search or scrape fallback is permitted"
    );
}

/// A stream that completes without a hosted search is a failed search,
/// not an empty one — and never a fallback.
#[tokio::test]
async fn grok_origin_websearch_without_sources_fails_strictly() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => terminal_only_sse(),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..grok_websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "grok-4.5", "claude-sonnet-4-5").await;

    assert!(body.contains("web_search_tool_result_error"));
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/responses"]
    );
    assert_eq!(anthropic_observed.lock().await.len(), 0);
}

/// A GPT origin keeps using alpha/search even when Grok routes exist, with
/// the Codex slug pin intact.
#[tokio::test]
async fn gpt_origin_still_uses_alpha_search_when_grok_routes_exist() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/alpha/search" => {
                let document: serde_json::Value = serde_json::from_slice(body).unwrap();
                assert_eq!(document["model"], "gpt-6-astra");
                alpha_results_response()
            }
            path => panic!("unexpected CPA path {path} (xAI search is Grok-only)"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let app = model_router::proxy::app(grok_websearch_config(cpa_address))
        .await
        .unwrap();
    let body = turn_then_subcall(app, "claude-gpt-test", "claude-sonnet-4-5").await;

    assert!(body.contains("https://bun.sh/blog"));
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/alpha/search"]
    );
}

/// A Claude subagent searching inside a Grok session is still answered by
/// Anthropic — the Grok carrier must not capture it.
#[tokio::test]
async fn claude_origin_under_a_grok_carrier_still_goes_to_anthropic() {
    fn anthropic(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        assert_eq!(parts.uri.path(), "/v1/messages");
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        assert_eq!(document["model"], "claude-haiku-4-5");
        Response::new(Body::from(
            serde_json::json!({
                "id": "msg_native", "type": "message", "role": "assistant",
                "model": "claude-haiku-4-5", "stop_reason": "end_turn",
                "content": [
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_n",
                     "content": [{"type": "web_search_result", "title": "Bun releases",
                                  "url": "https://bun.sh/blog"}]},
                ],
                "usage": {"input_tokens": 100, "output_tokens": 200},
            })
            .to_string(),
        ))
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!(
            "a Claude-origin search must not reach the routed upstream, got {}",
            parts.uri.path()
        );
    }
    let (cpa_address, _) = spawn_fake(cpa).await;
    let (anthropic_address, _) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..grok_websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "claude-haiku-4-5", "grok-4.5").await;
    assert!(body.contains("https://bun.sh/blog"));
    assert!(!body.contains("web_search_tool_result_error"));
}

/// After a TRANSIENT Anthropic failure the Claude-origin sub-call falls
/// through to the routed path — and must still never reach xAI, even though
/// the carrying route is Grok. This is the leak a carrier-based gate ships.
#[tokio::test]
async fn claude_origin_transient_anthropic_failure_under_a_grok_carrier_never_reaches_xai() {
    fn anthropic(_parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        let document: serde_json::Value = serde_json::from_slice(body).unwrap();
        if document.get("tool_choice").is_none() {
            return sse_response(websearch_tool_use_sse(), false);
        }
        // 503 is fallback-eligible: the router continues down the GPT path.
        let mut response = Response::new(Body::from("anthropic overloaded"));
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        response
    }
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        assert_ne!(
            parts.uri.path(),
            "/v1/responses",
            "a Claude-origin search must never reach xAI's search"
        );
        assert_eq!(parts.uri.path(), "/v1/alpha/search");
        alpha_results_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, _) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..grok_websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "claude-haiku-4-5", "grok-4.5").await;

    assert!(body.contains("https://bun.sh/blog"));
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/alpha/search"],
        "the fallback stays on the Codex search backend"
    );
}

/// A GPT origin carried by a DIFFERENT GPT route keeps using the
/// carrier's own arguments. Both routes are Codex-native and distinct, so the
/// alpha slug actually distinguishes them: substituting the origin for the
/// carrier would be visible.
#[tokio::test]
async fn gpt_origin_on_a_different_gpt_carrier_keeps_the_carriers_arguments() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/alpha/search" => {
                let document: serde_json::Value = serde_json::from_slice(body).unwrap();
                assert_eq!(
                    document["model"], "gpt-5.6-terra",
                    "the alpha call must carry the CARRIER's model, not the origin's"
                );
                alpha_results_response()
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let mut config = grok_websearch_config(cpa_address);
    // Two distinct Codex-native routes: `alpha_search_model` passes each
    // through unchanged, so origin and carrier stay tellable apart.
    config.models.push(ModelRoute {
        routing_id: "gpt-5.6-sol".to_string(),
        upstream: "cliproxy".to_string(),
        upstream_model: "gpt-5.6-sol".to_string(),
        display_name: "GPT-5.6 Sol".to_string(),
        ..Default::default()
    });
    config.models.push(ModelRoute {
        routing_id: "gpt-5.6-terra".to_string(),
        upstream: "cliproxy".to_string(),
        upstream_model: "gpt-5.6-terra".to_string(),
        display_name: "GPT-5.6 Terra".to_string(),
        ..Default::default()
    });
    let app = model_router::proxy::app(config).await.unwrap();

    // Sol subagent turn records the origin; the sub-call is carried by Terra.
    let body = turn_then_subcall(app, "gpt-5.6-sol", "gpt-5.6-terra").await;
    assert!(body.contains("https://bun.sh/blog"));
    // The synthesized answer is attributed to the carrying route, unchanged.
    assert!(body.contains(r#""model":"gpt-5.6-terra""#), "{body}");
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/alpha/search"],
        "a GPT origin never reaches xAI, whatever carries it"
    );
}

// ---- xAI search stream ownership ----

/// An SSE body whose tail is produced only if the consumer keeps reading:
/// the tail future sets `consumed` when polled, so a client that walked away
/// (or a spawned task that does nothing) cannot set it.
fn sse_with_observable_tail(
    chunks: Vec<Bytes>,
    head_delay: Duration,
    tail_delay: Duration,
    consumed: &'static std::sync::atomic::AtomicBool,
) -> Response {
    let head = futures_util::stream::once(async move {
        tokio::time::sleep(head_delay).await;
        Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b":warmup\n\n"))
    })
    .chain(futures_util::stream::iter(chunks.into_iter().map(Ok)));
    let tail = futures_util::stream::once(async move {
        tokio::time::sleep(tail_delay).await;
        consumed.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Bytes::from_static(
            b"event: x\ndata: {\"type\":\"response.completed\"}\n\n",
        ))
    });
    let mut response = Response::new(Body::from_stream(head.chain(tail)));
    response.headers_mut().insert(
        "content-type",
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn source_event(url: &str) -> Bytes {
    let data = serde_json::json!({"type": "response.output_item.done", "item": {
        "id": "ws_1", "type": "web_search_call", "status": "completed",
        "action": {"type": "search", "query": "bun release notes",
                   "sources": [{"type": "url", "url": url}]}}});
    Bytes::from(format!("event: x\ndata: {data}\n\n"))
}

async fn wait_for(flag: &std::sync::atomic::AtomicBool, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

static I1_TAIL_CONSUMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The answer arrives at the harvest, and the stream is
/// still read to its end afterwards. An empty spawned task would fail this.
#[tokio::test]
async fn grok_origin_search_is_answered_at_the_harvest_and_the_tail_is_consumed() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => sse_with_observable_tail(
                vec![source_event("https://bun.sh/blog")],
                Duration::ZERO,
                Duration::from_millis(300),
                &I1_TAIL_CONSUMED,
            ),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, _) = spawn_fake(cpa).await;
    let app = model_router::proxy::app(grok_websearch_config(cpa_address))
        .await
        .unwrap();
    // The router owns the search registry, so it must outlive the request the
    // way it does in a running gateway.
    let keepalive = app.clone();
    let body = turn_then_subcall(app, "grok-4.5", "claude-sonnet-4-5").await;

    assert!(body.contains("https://bun.sh/blog"));
    // Answered without waiting for the tail: the flag is set only once the
    // tail has been read, so a router that waited would see it set here.
    assert!(
        !I1_TAIL_CONSUMED.load(std::sync::atomic::Ordering::SeqCst),
        "the tail cannot have been consumed before it was sent"
    );
    // ...and the stream is still read to completion afterwards.
    assert!(
        wait_for(&I1_TAIL_CONSUMED, Duration::from_secs(5)).await,
        "the stream tail was never consumed: the connection was abandoned"
    );
    drop(keepalive);
}

static TIMEOUT_TAIL_CONSUMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The harvest deadline stops the handler WAITING; it must not cancel the
/// upstream read. Otherwise the timeout path recreates the very disconnect
/// that quarantines the child's xAI auth.
#[tokio::test]
async fn a_timed_out_search_still_reads_its_stream_to_the_end() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            // Sources arrive well after the handler's deadline.
            "/v1/responses" => sse_with_observable_tail(
                vec![source_event("https://bun.sh/blog")],
                Duration::from_millis(400),
                Duration::from_millis(200),
                &TIMEOUT_TAIL_CONSUMED,
            ),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, _) = spawn_fake(cpa).await;
    let app = model_router::proxy::app(Config {
        native_search: NativeSearchLimits {
            harvest_timeout: Duration::from_millis(100),
            ..Default::default()
        },
        ..grok_websearch_config(cpa_address)
    })
    .await
    .unwrap();
    let keepalive = app.clone();
    let body = turn_then_subcall(app, "grok-4.5", "claude-sonnet-4-5").await;

    // The client sees a failed search rather than a hung request...
    assert!(body.contains("web_search_tool_result_error"));
    assert!(body.contains("did not return sources in time"));
    // ...while the abandoned-looking stream is still read to its end.
    assert!(
        wait_for(&TIMEOUT_TAIL_CONSUMED, Duration::from_secs(5)).await,
        "the timeout cancelled the upstream read: this is the quarantine bug"
    );
    drop(keepalive);
}

static SHUTDOWN_TAIL_CONSUMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Shutdown waits for in-flight search streams instead of dropping them: a
/// dropped stream is a disconnect, and a disconnect is what quarantines the
/// child's xAI auth — at the worst possible moment, a restart.
#[tokio::test]
async fn shutdown_waits_for_in_flight_search_streams() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => sse_with_observable_tail(
                vec![source_event("https://bun.sh/blog")],
                Duration::ZERO,
                Duration::from_millis(400),
                &SHUTDOWN_TAIL_CONSUMED,
            ),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(model_router::proxy::serve_listener_with_drain(
        listener,
        grok_websearch_config(cpa_address),
        None,
        async move {
            let _ = shutdown_rx.await;
        },
        Duration::from_secs(10),
    ));

    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let turn = client
        .post(format!("{base}/v1/messages"))
        .header("content-type", "application/json")
        .body(websearch_declaring_body("grok-4.5"))
        .send()
        .await
        .unwrap();
    let mut stream = turn.bytes_stream();
    let mut seen = String::new();
    while let Some(chunk) = stream.next().await {
        seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        if seen.contains("content_block_stop") {
            break;
        }
    }
    drop(stream);
    let subcall = client
        .post(format!("{base}/v1/messages"))
        .header("content-type", "application/json")
        .body(origin_subcall_body("claude-sonnet-4-5"))
        .send()
        .await
        .unwrap();
    assert!(
        subcall
            .text()
            .await
            .unwrap()
            .contains("https://bun.sh/blog")
    );
    // The client has its answer while the stream tail is still outstanding.
    assert!(!SHUTDOWN_TAIL_CONSUMED.load(std::sync::atomic::Ordering::SeqCst));

    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(
        SHUTDOWN_TAIL_CONSUMED.load(std::sync::atomic::Ordering::SeqCst),
        "shutdown returned while a search stream was still open"
    );
    // Shutting down opened nothing new; it only finished what was running.
    let observed = cpa_observed.lock().await;
    assert_eq!(
        observed_paths(&observed)
            .iter()
            .filter(|path| **path == "/v1/responses")
            .count(),
        1
    );
}

/// A route that asked for sub-provider pinning and has no selection is
/// answered by the router itself, with the reason, before any upstream.
#[tokio::test]
async fn an_unpinned_route_is_refused_with_the_reason() {
    // The only way a real install reaches this state: a provider model that
    // asks for a pin and has no cached selection, copied onto the generated
    // route by `prepare`.
    let config: Config = toml::from_str(
        r#"
            [upstreams.cliproxy]
            mode = "stub"
            [[openai-providers]]
            name = "openrouter"
            base-url = "https://openrouter.ai/api/v1"
            [[openai-providers.models]]
            name = "z-ai/glm-5.2"
            routing-id = "glm-5.2"
            display-name = "GLM-5.2"
            min-context-window = 2000000
        "#,
    )
    .unwrap();
    let app = model_router::proxy::app(config).await.unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"glm-5.2","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("not_found_error"), "{body}");
    assert!(body.contains("glm-5.2 is not served"), "{body}");
    assert!(body.contains("at least 2000000 tokens"), "{body}");
}

/// The `field` pattern Claude Code 2.1.265+ ships on the Artifact tool; the
/// Codex validator rejects the whole request over its `\p{…}` escapes.
const ARTIFACT_FIELD_PATTERN: &str = r#"^(?!__.*__$)[^\p{Cc}\p{Cf}\p{Zl}\p{Zp}"\\./[\]]{1,200}$"#;
/// Its sibling identifier pattern: lookahead only, accepted upstream.
const ARTIFACT_DOC_ID_PATTERN: &str = r"^(?!\.\.?(?:\/|$))[A-Za-z0-9_\-.~:@+]{1,200}$";

fn artifact_tools() -> serde_json::Value {
    serde_json::json!([
        {"name": "Glob", "description": "Find files",
         "input_schema": {"type": "object",
             "properties": {"pattern": {"type": "string", "description": "glob"}}}},
        {"name": "Artifact", "description": "Publish",
         "input_schema": {"type": "object",
             "properties": {
                 "field": {"type": "string", "pattern": ARTIFACT_FIELD_PATTERN},
                 "doc_id": {"type": "string", "pattern": ARTIFACT_DOC_ID_PATTERN}},
             "required": ["field"]}},
    ])
}

fn plain_message_response() -> Response {
    Response::new(Body::from(
        serde_json::json!({
            "id": "msg_upstream", "type": "message", "role": "assistant",
            "model": "gpt-test", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "pong"}],
            "usage": {"input_tokens": 10, "output_tokens": 1},
        })
        .to_string(),
    ))
}

/// A GPT turn reaches the upstream with the Artifact tool still declared and
/// only its unportable `pattern` removed, alongside the identity block; the
/// same tools on a Claude turn pass through byte-exact.
#[tokio::test]
async fn gpt_turn_drops_tool_schema_patterns_the_codex_validator_rejects() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(handler).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    // Non-canonical spelling (whitespace, key order, a `\u` escape) so a
    // parse-and-reserialize of a Claude turn would show up in the byte check.
    let body_for = |model: &str| {
        format!(
            "{{ \"tools\" : {},\n  \"messages\":[{{\"role\":\"user\",\"content\":\"Reply with exactly: p\\u006fng\"}}],\
             \"max_tokens\": 16, \"model\":\"{model}\" }}",
            artifact_tools()
        )
    };

    let gpt_body = body_for("claude-gpt-test");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(gpt_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = cpa_observed.lock().await;
    assert_eq!(forwarded.len(), 1);
    let document: serde_json::Value = serde_json::from_slice(&forwarded[0].body).unwrap();
    assert_eq!(document["model"], "gpt-test");
    assert!(
        document["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("GPT Test")
    );
    let tools = document["tools"].as_array().unwrap();
    assert_eq!(
        tools[0],
        artifact_tools()[0],
        "Glob's `pattern` property is data"
    );
    assert_eq!(tools[1]["name"], "Artifact");
    let properties = &tools[1]["input_schema"]["properties"];
    assert!(properties["field"].get("pattern").is_none());
    assert_eq!(properties["field"]["type"], "string");
    assert_eq!(properties["doc_id"]["pattern"], ARTIFACT_DOC_ID_PATTERN);
    assert_eq!(
        tools[1]["input_schema"]["required"],
        serde_json::json!(["field"])
    );
    drop(forwarded);

    let claude_body = body_for("claude-sonnet-4-5");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(claude_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = anthropic_observed.lock().await;
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].body, claude_body.as_bytes());
}

/// A tool Claude Code adds mid-conversation arrives as a system message's
/// `tool_addition` block, which the child drops: a GPT turn gets it declared
/// in `tools[]` (through the schema shim too) and loses the deferred-tool
/// placeholder, while a Claude turn is forwarded byte for byte.
#[tokio::test]
async fn gpt_turn_declares_tools_added_mid_conversation() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(handler).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let late_artifact = artifact_tools()[1].clone();
    let body_for = |model: &str| {
        serde_json::to_string(&serde_json::json!({
            "model": model,
            "max_tokens": 16,
            "tools": [
                {"name": "Bash", "input_schema": {"type": "object", "properties": {}}},
                {
                    "name": "DeferredToolPlaceholder",
                    "description": "Reserved placeholder that keeps deferred tool loading active; never call this tool.",
                    "input_schema": {"type": "object", "properties": {}},
                    "defer_loading": true,
                },
            ],
            "messages": [
                {"role": "user", "content": "review this"},
                {"role": "system", "content": [
                    {"type": "text", "text": "Artifact is now available."},
                    {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": late_artifact}},
                ]},
                {"role": "user", "content": "go on"},
            ],
        }))
        .unwrap()
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(body_for("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = cpa_observed.lock().await;
    assert_eq!(forwarded.len(), 1);
    let document: serde_json::Value = serde_json::from_slice(&forwarded[0].body).unwrap();
    let tools = document["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Bash", "Artifact"]);
    assert!(
        tools[1]["input_schema"]["properties"]["field"]
            .get("pattern")
            .is_none()
    );
    assert_eq!(
        document["messages"][1],
        serde_json::json!({"role": "system", "content": [{"type": "text", "text": "Artifact is now available."}]})
    );
    drop(forwarded);

    let claude_body = body_for("claude-sonnet-4-5");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(claude_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = anthropic_observed.lock().await;
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].body, claude_body.as_bytes());
}

/// A compaction request keeps its tools but must be answered in text:
/// Claude Code's compaction header turns tool use off, and only there, and
/// only for Codex-native GPT models and Grok. An openai-compat host, or a
/// hand-written `gpt` route to another model, gets the request as sent, and
/// Muse accepts no `tool_choice` but `auto`.
#[tokio::test]
async fn compaction_is_answered_without_tools_only_on_codex_native_gpt_and_grok() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let mut config = muse_config(cpa_address, Some(auth.path()));
    config.models.extend([
        ModelRoute {
            routing_id: "gpt-6-astra".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-6-astra".to_string(),
            display_name: "GPT-6 Astra".to_string(),
            ..Default::default()
        },
        ModelRoute {
            routing_id: "kimi-k3".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "openai-compat--kimi-k3".to_string(),
            display_name: "Kimi K3".to_string(),
            family: model_router::config::ModelFamily::OpenAiCompat,
            ..Default::default()
        },
    ]);
    let app = model_router::proxy::app(config).await.unwrap();
    let cases = [
        ("gpt-6-astra", true),
        ("grok-4.5", true),
        // A hand-written `gpt` route to a model outside the Codex list.
        ("claude-gpt-test", false),
        ("kimi-k3", false),
        ("muse-wonder-1.0", false),
    ];
    for (model, _) in cases {
        let body = serde_json::json!({
            "model": model, "max_tokens": 16, "tools": artifact_tools(),
            "messages": [{"role": "user", "content": "Summarize the conversation."}],
        })
        .to_string();
        for compaction in [true, false] {
            let headers: &[(&str, &str)] = if compaction {
                &[("x-claude-code-compaction", "auto")]
            } else {
                &[]
            };
            let (status, response) = send(&app, body.clone(), headers).await;
            assert_eq!(status, StatusCode::OK, "{model}: {response}");
        }
    }
    let forwarded = cpa_observed.lock().await;
    assert_eq!(forwarded.len(), cases.len() * 2);
    for ((model, forced), pair) in cases.iter().zip(forwarded.chunks(2)) {
        let compaction: serde_json::Value = serde_json::from_slice(&pair[0].body).unwrap();
        let turn: serde_json::Value = serde_json::from_slice(&pair[1].body).unwrap();
        if *forced {
            assert_eq!(
                compaction["tool_choice"],
                serde_json::json!({"type": "none"}),
                "{model}"
            );
        } else {
            assert!(
                compaction.get("tool_choice").is_none(),
                "{model}: {compaction}"
            );
        }
        assert_eq!(compaction["tools"].as_array().unwrap().len(), 2, "{model}");
        assert!(turn.get("tool_choice").is_none(), "{model}");
    }
}

/// A forced WebSearch sub-call from a GPT-origin agent, arriving on the
/// Claude branch with the caller's full tool set, is forwarded to the GPT
/// upstream by the legacy path when alpha search fails — with the same
/// schema rewrite, and without an identity block.
#[tokio::test]
async fn gpt_origin_forced_subcall_fallback_drops_patterns_without_identity() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/alpha/search" => {
                let mut response = Response::new(Body::from("search backend down"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
            "/v1/messages" if body.windows(20).any(|w| w == b"web_search_20250305\"") => {
                plain_message_response()
            }
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..websearch_config(cpa_address)
    };
    let app = model_router::proxy::app(config).await.unwrap();

    let gpt_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(websearch_declaring_body("claude-gpt-test")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gpt_turn.status(), StatusCode::OK);
    let mut gpt_stream = gpt_turn.into_body().into_data_stream();
    read_until(&mut gpt_stream, "content_block_stop").await;

    let mut subcall_body: serde_json::Value =
        serde_json::from_str(&origin_subcall_body("claude-sonnet-4-5")).unwrap();
    subcall_body["tool_choice"] = serde_json::json!({"type": "tool", "name": "web_search"});
    let tools = subcall_body["tools"].as_array_mut().unwrap();
    tools.extend(artifact_tools().as_array().unwrap().iter().cloned());
    let subcall = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(subcall_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(subcall.status(), StatusCode::OK);
    let body = to_bytes(subcall.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("event: message_stop"));

    assert_eq!(anthropic_observed.lock().await.len(), 0);
    let cpa_requests = cpa_observed.lock().await;
    assert_eq!(
        cpa_requests
            .iter()
            .map(|request| request.uri.as_str())
            .collect::<Vec<_>>(),
        ["/v1/messages", "/v1/alpha/search", "/v1/messages"]
    );
    let document: serde_json::Value = serde_json::from_slice(&cpa_requests[2].body).unwrap();
    assert_eq!(document["model"], "gpt-test");
    assert_eq!(document["stream"], false);
    assert_eq!(
        document["system"][0]["text"], "You are an assistant for performing a web search tool use",
        "no identity block on a side call"
    );
    let tools = document["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "web_search");
    assert_eq!(tools[2]["name"], "Artifact");
    let properties = &tools[2]["input_schema"]["properties"];
    assert!(properties["field"].get("pattern").is_none());
    assert_eq!(properties["doc_id"]["pattern"], ARTIFACT_DOC_ID_PATTERN);
}

// ---- Muse (Meta) family ----

/// A fresh auth dir holding one Meta login, `login`.
fn muse_auth_dir_with(login: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("meta-a@example.com-0123456789abcdef.json"),
        login,
    )
    .unwrap();
    dir
}

/// A fresh auth dir holding a Meta login as `CLIProxyAPI` mints it (no
/// `headers`).
fn muse_auth_dir() -> tempfile::TempDir {
    muse_auth_dir_with(r#"{"type":"meta","auth_kind":"oauth","api_key":"k","access_token":"k"}"#)
}

/// The built-in Muse routes beside Grok and GPT ones, with the Muse gate
/// reading `auth_dir`.
fn muse_config(fake_address: std::net::SocketAddr, auth_dir: Option<&std::path::Path>) -> Config {
    let mut config = grok_websearch_config(fake_address);
    config.muse.enabled = true;
    config.auth_dir = auth_dir.map(std::path::Path::to_path_buf);
    config
}

/// Sends one `/v1/messages` request and returns its status and body.
async fn send(app: &axum::Router, body: String, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn effort_body(model: &str) -> String {
    serde_json::json!({
        "model": model, "max_tokens": 16,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
        "messages": [{"role": "user", "content": "hello"}],
    })
    .to_string()
}

/// The Meta path maps `output_config.effort` to `reasoning.effort` itself,
/// so a Muse route forwards it untouched under the bare model ID — while
/// xAI, which drops the field, gets the effort in the model ID.
#[tokio::test]
async fn muse_route_forwards_effort_as_output_config_and_grok_as_a_suffix() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();

    let (status, _) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, effort_body("grok-4.5"), &[]).await;
    assert_eq!(status, StatusCode::OK);

    let forwarded = cpa_observed.lock().await;
    let documents: Vec<serde_json::Value> = forwarded
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(documents[0]["model"], "muse-wonder-1.0");
    assert_eq!(documents[0]["output_config"]["effort"], "high");
    assert_eq!(documents[0]["thinking"]["type"], "adaptive");
    assert!(
        documents[0]["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Muse Wonder"),
        "{}",
        documents[0]["system"]
    );
    assert_eq!(documents[1]["model"], "grok-4.5(high)");
}

/// Without a Meta login, or while one carries the Contributor header, a Muse
/// request is refused visibly — in Anthropic's error format, naming the fix —
/// and nothing reaches the child. Other families are unaffected.
#[tokio::test]
async fn muse_requests_are_refused_without_a_meta_login_free_of_the_contributor_header() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = tempfile::tempdir().unwrap();
    let refused = |body: &str, needle: &str| {
        let error: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(error["type"], "error", "{body}");
        assert_eq!(error["error"]["type"], "invalid_request_error", "{body}");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("Contributor-mode"), "{message}");
        assert!(message.contains(needle), "{message}");
    };

    // No auth dir to vouch for the login at all (a non-managed upstream).
    let app = model_router::proxy::app(muse_config(cpa_address, None))
        .await
        .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    refused(&body, "managed");

    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    // No Meta login yet.
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    refused(&body, "model-router login muse");

    // A login carrying the Contributor header, even set to "0". The gate
    // reads the directory per request, so no restart is involved.
    let login = auth.path().join("meta-a@example.com-0123456789abcdef.json");
    std::fs::write(
        &login,
        r#"{"type":"meta","api_key":"k","headers":{"X-Tbh-Contributor-Mode":"0"}}"#,
    )
    .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    refused(&body, "remove it from the login's");
    assert!(
        cpa_observed.lock().await.is_empty(),
        "a refused request must not reach the child"
    );

    // GPT and Grok routes never wait on the Meta login.
    let (status, _) = send(&app, effort_body("claude-gpt-test"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, effort_body("grok-4.5"), &[]).await;
    assert_eq!(status, StatusCode::OK);

    // The child may still hold the header after the fix, so the refusal
    // stays until a restart.
    let before = r#"{"type":"meta","api_key":"k"}"#;
    std::fs::write(&login, before).unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    refused(&body, "model-router service restart");

    // After the restart the login as the child mints it lets the request
    // through, under the plain model ID, and the router adds no Contributor
    // header of its own.
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let (status, _) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let forwarded = cpa_observed.lock().await;
    assert_eq!(forwarded.len(), 3);
    let document: serde_json::Value = serde_json::from_slice(&forwarded[2].body).unwrap();
    assert_eq!(document["model"], "muse-wonder-1.0");
    assert!(
        !forwarded[2]
            .headers
            .keys()
            .any(|name| name.as_str().starts_with("x-tbh-")),
        "{:?}",
        forwarded[2].headers
    );
    assert_eq!(std::fs::read_to_string(&login).unwrap(), before);
}

/// The Contributor trigger header the router sends the child in Contributor
/// mode (`muse::CONTRIBUTOR_TRIGGER`).
const CONTRIBUTOR_TRIGGER: &str = "x-model-router-muse-contributor";

/// A Meta login carrying the Contributor relay, as `sync_relay` writes it.
const RELAYED_META_LOGIN: &str = r#"{"type":"meta","api_key":"k","headers":{"X-Tbh-Contributor-Mode":"$x-model-router-muse-contributor"}}"#;

/// With `[muse] contributor = true`, a Muse request goes out under the
/// `-contributor` model ID with the trigger the login's relay turns into
/// Muse Code's header, once every Meta login relays it. Standard mode on the
/// same relayed login refuses, and never sends the trigger, not even one a
/// client supplied.
#[tokio::test]
async fn contributor_mode_sends_the_contributor_id_and_trigger_and_standard_mode_never_does() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let contributor = |auth: &std::path::Path| {
        let mut config = muse_config(cpa_address, Some(auth));
        config.muse.contributor = true;
        config
    };

    // A minted login has no relay yet: refused, naming the fix.
    let app = model_router::proxy::app(contributor(auth.path()))
        .await
        .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Contributor mode"), "{body}");
    assert!(body.contains("service restart"), "{body}");
    assert!(cpa_observed.lock().await.is_empty());

    // With the relay: the routing ID stays, the upstream ID is the
    // -contributor one, and the trigger is on.
    let relayed = muse_auth_dir_with(RELAYED_META_LOGIN);
    let app = model_router::proxy::app(contributor(relayed.path()))
        .await
        .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-full-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Other families carry the trigger too; only a Meta login relays it.
    let (status, _) = send(&app, effort_body("grok-4.5"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    {
        let forwarded = cpa_observed.lock().await;
        assert_eq!(forwarded.len(), 2);
        let document: serde_json::Value = serde_json::from_slice(&forwarded[0].body).unwrap();
        assert_eq!(document["model"], "muse-wonder-full-1.0-contributor");
        assert_eq!(forwarded[0].headers[CONTRIBUTOR_TRIGGER], "1");
        assert_eq!(forwarded[1].headers[CONTRIBUTOR_TRIGGER], "1");
        assert!(
            !forwarded[0]
                .headers
                .keys()
                .any(|name| name.as_str().starts_with("x-tbh-")),
            "the header itself is the child's to set"
        );
    }

    // Switched back to Standard mode without the restart that removes the
    // relay: Muse is refused, and nothing the router sends carries the
    // trigger, not even a client's own.
    let app = model_router::proxy::app(muse_config(cpa_address, Some(relayed.path())))
        .await
        .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("remove the relay"), "{body}");
    let (status, _) = send(&app, effort_body("grok-4.5"), &[(CONTRIBUTOR_TRIGGER, "1")]).await;
    assert_eq!(status, StatusCode::OK);
    // Once the relay is gone, Muse goes out in Standard mode again.
    let (status, _) = send(
        &app,
        effort_body("muse-wonder-1.0"),
        &[(CONTRIBUTOR_TRIGGER, "1")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    std::fs::write(
        relayed
            .path()
            .join("meta-a@example.com-0123456789abcdef.json"),
        r#"{"type":"meta","api_key":"k"}"#,
    )
    .unwrap();
    let (status, _) = send(
        &app,
        effort_body("muse-wonder-1.0"),
        &[(CONTRIBUTOR_TRIGGER, "1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let forwarded = cpa_observed.lock().await;
    assert_eq!(forwarded.len(), 4);
    for request in &forwarded[2..] {
        assert!(
            !request.headers.contains_key(CONTRIBUTOR_TRIGGER),
            "{:?}",
            request.headers
        );
    }
    let document: serde_json::Value = serde_json::from_slice(&forwarded[3].body).unwrap();
    assert_eq!(document["model"], "muse-wonder-1.0");
}

/// A Muse agent's Meta search in Contributor mode is Contributor traffic
/// too: the -contributor ID, with the trigger.
#[tokio::test]
async fn contributor_mode_meta_search_uses_the_contributor_id_and_trigger() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => meta_search_sse(false),
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let auth = muse_auth_dir_with(RELAYED_META_LOGIN);
    let mut config = muse_config(cpa_address, Some(auth.path()));
    config.muse.contributor = true;
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "muse-wonder-1.0", "muse-wonder-1.0").await;
    assert!(
        body.contains("https://d3x-solutions.com/meta-muse-wonder-full-muse-code-only/"),
        "{body}"
    );
    let forwarded = cpa_observed.lock().await;
    let search = forwarded
        .iter()
        .find(|request| request.uri == "/v1/responses")
        .expect("a Meta search");
    let document: serde_json::Value = serde_json::from_slice(&search.body).unwrap();
    assert_eq!(document["model"], "muse-wonder-1.0-contributor");
    assert_eq!(search.headers[CONTRIBUTOR_TRIGGER], "1");
}

/// The byte-faithful Meta search stream (see `src/testdata/README.md`).
const META_SEARCH_SSE: &str = include_str!("../src/testdata/meta-websearch-basic.sse");

/// The capture as a child would deliver it: in 1 KB pieces.
fn meta_search_sse(hold_open: bool) -> Response {
    let chunks = META_SEARCH_SSE
        .as_bytes()
        .chunks(1024)
        .map(Bytes::copy_from_slice)
        .collect();
    sse_response(chunks, hold_open)
}

/// The Meta search body, as measured to work.
fn assert_meta_search_body(parts: &axum::http::request::Parts, body: &Bytes) {
    let document: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(document["model"], "muse-wonder-1.0");
    assert_eq!(document["tools"][0]["type"], "web_search");
    assert_eq!(document["tool_choice"], "auto");
    assert_eq!(document["max_tool_calls"], 1);
    assert_eq!(document["include"][0], "web_search_call.action.sources");
    assert_eq!(document["stream"], true);
    assert!(
        document["input"][0]["content"]
            .as_str()
            .unwrap()
            .contains("bun release notes")
    );
    assert_eq!(parts.headers["authorization"], "Bearer gateway-secret");
}

/// A Muse agent's search is answered from Meta's hosted `web_search` on the
/// child — never Codex's alpha search, xAI's, or Anthropic — whichever model
/// carries the sub-call.
#[tokio::test]
async fn muse_origin_websearch_is_answered_from_meta_native_search() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                assert_meta_search_body(parts, body);
                meta_search_sse(false)
            }
            path => panic!("a Muse-origin search must not reach {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let auth = muse_auth_dir();
    // Carried by a Claude model (the Claude branch), and by GPT and Grok
    // routes (the routed branch): the origin decides every time.
    for carrier in ["claude-sonnet-4-5", "claude-gpt-test", "grok-4.5"] {
        let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
        let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
        let config = Config {
            anthropic_upstream_base: format!("http://{anthropic_address}"),
            ..muse_config(cpa_address, Some(auth.path()))
        };
        let app = model_router::proxy::app(config).await.unwrap();
        let body = turn_then_subcall(app, "muse-wonder-1.0", carrier).await;

        assert!(
            body.contains(r#""type":"web_search_tool_result""#),
            "{carrier}"
        );
        assert!(
            !body.contains("web_search_tool_result_error"),
            "{carrier}: {body}"
        );
        assert!(
            body.contains("https://d3x-solutions.com/meta-muse-wonder-full-muse-code-only/"),
            "{carrier}: {body}"
        );
        assert!(
            body.contains("https://digitalstrategy-ai.com/2026/10/13/ai-news-today-maia-300/"),
            "{carrier}: {body}"
        );
        // Meta's sources carry no titles: the URL is its own label, and the
        // answer text (where the titles are) never reaches the result.
        assert!(
            body.contains(r#""title":"https://ai-tldr.dev/releases/meta-muse-wonder/""#),
            "{carrier}: {body}"
        );
        assert!(!body.contains("D3X Solutions LABS"), "{carrier}");
        assert_eq!(anthropic_observed.lock().await.len(), 0, "{carrier}");
        assert_eq!(
            observed_paths(&cpa_observed.lock().await),
            ["/v1/messages", "/v1/responses"],
            "{carrier}: the search goes to the child's /v1/responses and nowhere else"
        );
    }
}

/// A Meta search that errors is reported as a failed search, naming Meta. It
/// must not fall back to alpha search, xAI, the origin route, or Anthropic.
#[tokio::test]
async fn muse_origin_websearch_failure_is_visible_and_never_falls_back() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                let mut response = Response::new(Body::from("upstream exploded"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
    let auth = muse_auth_dir();
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        ..muse_config(cpa_address, Some(auth.path()))
    };
    let app = model_router::proxy::app(config).await.unwrap();
    let body = turn_then_subcall(app, "muse-wonder-1.0", "claude-sonnet-4-5").await;

    assert!(body.contains("web_search_tool_result_error"), "{body}");
    assert!(body.contains(r#""error_code":"unavailable""#), "{body}");
    assert!(body.contains("Meta search returned HTTP 500"), "{body}");
    assert!(!body.contains("web_search_requests"));
    assert_eq!(anthropic_observed.lock().await.len(), 0);
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/responses"],
        "no alpha/search, xAI or scrape fallback is permitted"
    );
}

/// The sub-call carrying a Muse agent's search may be a Claude request the
/// Muse gate never saw, so the Meta search passes the gate itself: once a
/// Meta login carries the Contributor header, nothing reaches Meta.
#[tokio::test]
async fn a_muse_origin_search_is_refused_by_the_muse_gate() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), false),
            path => panic!("a refused search must not reach {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    // The Muse turn passes the gate and records the origin...
    let (status, _) = send(&app, websearch_declaring_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    // ...then the login gains the header before the search arrives.
    std::fs::write(
        auth.path().join("meta-a@example.com-0123456789abcdef.json"),
        r#"{"type":"meta","api_key":"k","headers":{"X-Tbh-Contributor-Mode":"0"}}"#,
    )
    .unwrap();
    let (status, body) = send(&app, origin_subcall_body("claude-sonnet-4-5"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("web_search_tool_result_error"), "{body}");
    assert!(body.contains("Contributor-mode"), "{body}");
    assert_eq!(observed_paths(&cpa_observed.lock().await), ["/v1/messages"]);
}

static META_TAIL_CONSUMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The answer arrives at the harvest, and the rest of Meta's stream is still
/// read to its end, as xAI's is.
#[tokio::test]
async fn a_meta_search_is_answered_at_the_harvest_and_the_tail_is_consumed() {
    fn cpa(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                // The capture up to and including the completed search item
                // (the first event carrying sources).
                let item = META_SEARCH_SSE.find(r#""sources":["#).unwrap();
                let end = item + META_SEARCH_SSE[item..].find("\n\n").unwrap() + 2;
                sse_with_observable_tail(
                    vec![Bytes::from_static(&META_SEARCH_SSE.as_bytes()[..end])],
                    Duration::ZERO,
                    Duration::from_millis(300),
                    &META_TAIL_CONSUMED,
                )
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, _) = spawn_fake(cpa).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let keepalive = app.clone();
    let body = turn_then_subcall(app, "muse-wonder-1.0", "claude-sonnet-4-5").await;
    assert!(
        body.contains("https://cellcog.ai/blog/muse-wonder/"),
        "{body}"
    );
    assert!(
        wait_for(&META_TAIL_CONSUMED, Duration::from_secs(5)).await,
        "the stream tail was never consumed: the connection was abandoned"
    );
    drop(keepalive);
}

/// With no observed origin, a sub-call carried by a Muse route is the Muse
/// agent's own: Meta's search answers it, not alpha search.
#[tokio::test]
async fn an_uncorrelated_subcall_on_a_muse_carrier_is_answered_by_meta() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/responses" => {
                assert_meta_search_body(parts, body);
                meta_search_sse(false)
            }
            path => panic!("only Meta's search may be reached, got {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let (status, body) = send(&app, origin_subcall_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("web_search_tool_result_error"), "{body}");
    assert!(body.contains("https://www.datacamp.com/blog/"), "{body}");
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/responses"]
    );
}

/// `[web-search] mode = "off"` stops the GPT-side interception only: a Muse
/// agent's search still goes to Meta's search whatever carries it, never to
/// Anthropic or Codex, while a GPT agent's search passes through untouched.
#[tokio::test]
async fn muse_origin_websearch_stays_with_meta_when_search_is_off() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/responses" => {
                assert_meta_search_body(parts, body);
                meta_search_sse(false)
            }
            path => panic!("a Muse-origin search must not reach {path}"),
        }
    }
    fn anthropic(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        panic!("Anthropic must not be called, got {}", parts.uri.path());
    }
    fn passthrough(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), false),
            path => panic!("unexpected path {path}"),
        }
    }
    let auth = muse_auth_dir();
    let off = || model_router::config::WebSearchConfig {
        mode: model_router::config::WebSearchMode::Off,
    };
    for carrier in ["claude-sonnet-4-5", "claude-gpt-test"] {
        let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
        let (anthropic_address, anthropic_observed) = spawn_fake(anthropic).await;
        let config = Config {
            anthropic_upstream_base: format!("http://{anthropic_address}"),
            web_search: off(),
            ..muse_config(cpa_address, Some(auth.path()))
        };
        let app = model_router::proxy::app(config).await.unwrap();
        let body = turn_then_subcall(app, "muse-wonder-1.0", carrier).await;
        assert!(
            body.contains("https://d3x-solutions.com/meta-muse-wonder-full-muse-code-only/"),
            "{carrier}: {body}"
        );
        assert_eq!(anthropic_observed.lock().await.len(), 0, "{carrier}");
        assert_eq!(
            observed_paths(&cpa_observed.lock().await),
            ["/v1/messages", "/v1/responses"],
            "{carrier}"
        );
    }

    // A GPT agent's search under `off` is still not intercepted.
    let (address, observed) = spawn_fake(passthrough).await;
    let config = Config {
        anthropic_upstream_base: format!("http://{address}"),
        web_search: off(),
        ..muse_config(address, Some(auth.path()))
    };
    let app = model_router::proxy::app(config).await.unwrap();
    turn_then_subcall(app, "claude-gpt-test", "claude-sonnet-4-5").await;
    assert_eq!(
        observed_paths(&observed.lock().await),
        ["/v1/messages", "/v1/messages"]
    );
}

/// A forced search carrying the caller's other tools, on a Muse carrier: the
/// Muse rewrite drops the forced `tool_choice`, so detection must read the
/// inbound body or the sub-call slips past Meta's search.
#[tokio::test]
async fn a_forced_subcall_with_other_tools_on_a_muse_carrier_is_answered_by_meta() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/responses" => {
                assert_meta_search_body(parts, body);
                meta_search_sse(false)
            }
            path => panic!("only Meta's search may be reached, got {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let mut subcall: serde_json::Value =
        serde_json::from_str(&origin_subcall_body("muse-wonder-1.0")).unwrap();
    subcall["tool_choice"] = serde_json::json!({"type": "tool", "name": "web_search"});
    let tools = subcall["tools"].as_array_mut().unwrap();
    tools.extend(artifact_tools().as_array().unwrap().iter().cloned());
    let (status, body) = send(&app, subcall.to_string(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("web_search_tool_result_error"), "{body}");
    assert!(body.contains("https://www.datacamp.com/blog/"), "{body}");
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/responses"]
    );
}

/// A GPT agent searching from a Muse session keeps its own backend.
#[tokio::test]
async fn gpt_origin_on_a_muse_carrier_still_uses_alpha_search() {
    fn cpa(parts: &axum::http::request::Parts, body: &Bytes) -> Response {
        match parts.uri.path() {
            "/v1/messages" => sse_response(websearch_tool_use_sse(), true),
            "/v1/alpha/search" => {
                let document: serde_json::Value = serde_json::from_slice(body).unwrap();
                assert_eq!(document["model"], "gpt-6-astra");
                alpha_results_response()
            }
            path => panic!("unexpected CPA path {path}"),
        }
    }
    let (cpa_address, cpa_observed) = spawn_fake(cpa).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let body = turn_then_subcall(app, "claude-gpt-test", "muse-wonder-1.0").await;
    assert!(body.contains("https://bun.sh/blog"), "{body}");
    assert_eq!(
        observed_paths(&cpa_observed.lock().await),
        ["/v1/messages", "/v1/alpha/search"]
    );
}

/// What `CLIProxyAPI` 8.0.30 delivered for an over-window Muse request
/// (2026-10-15): Meta's generic invalid-parameters error, which Meta also
/// sends for other invalid parameters.
const META_OVERFLOW_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"The request contains invalid parameters. Check the request body for any errors or inconsistencies."}}"#;

/// Meta's generic error becomes Claude Code's overflow error only when the
/// request's own estimate is near the route's window; on a small request it
/// is some other invalid parameter and reaches the client untouched. Other
/// dialects never claim it.
#[tokio::test]
async fn muse_overflow_is_translated_only_for_a_request_near_the_window() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        let mut response = Response::new(Body::from(META_OVERFLOW_BODY));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        response
    }
    let (cpa_address, _) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let mut config = muse_config(cpa_address, Some(auth.path()));
    // A Muse route with a 1,000-token window, so a near-window request stays
    // small enough to tokenize quickly.
    config.models.push(ModelRoute {
        routing_id: "muse-small-window".to_string(),
        upstream: "cliproxy".to_string(),
        upstream_model: "muse-wonder-1.0".to_string(),
        display_name: "Muse Small Window".to_string(),
        family: model_router::config::ModelFamily::Muse,
        context_window: Some(1_000),
        ..Default::default()
    });
    let app = model_router::proxy::app(config).await.unwrap();
    let request = |model: &str, text: &str, stream: bool| {
        serde_json::json!({
            "model": model, "max_tokens": 16, "stream": stream,
            "messages": [{"role": "user", "content": text}],
        })
        .to_string()
    };
    // ~3 tokens per number: ~1,800 tokens, past the 1,000-token window.
    let large = "123456 ".repeat(600);

    for stream in [false, true] {
        let (status, body) = send(&app, request("muse-small-window", &large, stream), &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(envelope["error"]["type"], "invalid_request_error");
        let message = envelope["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("prompt is too long: "), "{message}");
        assert!(message.ends_with(" tokens > 1000 maximum"), "{message}");
    }
    // A small request on the same route, and on the built-in route with its
    // real ~1M window: some other invalid parameter, passed through.
    for model in ["muse-small-window", "muse-wonder-1.0"] {
        let (status, body) = send(&app, request(model, "hello", false), &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, META_OVERFLOW_BODY, "{model}");
    }
    // The xAI dialect does not speak Meta's phrase, whatever the size.
    let (status, body) = send(&app, request("grok-4.5", &large, false), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, META_OVERFLOW_BODY);
}

/// Meta accepts only `tool_choice: auto`: a Muse compaction keeps its tools
/// with no forced choice, and a forced choice from the client is dropped.
#[tokio::test]
async fn muse_requests_never_carry_a_tool_choice_but_auto() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let body = serde_json::json!({
        "model": "muse-wonder-1.0", "max_tokens": 16, "tools": artifact_tools(),
        "messages": [{"role": "user", "content": "Summarize the conversation."}],
    })
    .to_string();
    let (status, _) = send(&app, body.clone(), &[("x-claude-code-compaction", "auto")]).await;
    assert_eq!(status, StatusCode::OK);
    let mut forced: serde_json::Value = serde_json::from_str(&body).unwrap();
    forced["tool_choice"] = serde_json::json!({"type": "any"});
    let (status, _) = send(&app, forced.to_string(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let mut auto: serde_json::Value = serde_json::from_str(&body).unwrap();
    auto["tool_choice"] = serde_json::json!({"type": "auto"});
    let (status, _) = send(&app, auto.to_string(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let forwarded = cpa_observed.lock().await;
    let documents: Vec<serde_json::Value> = forwarded
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(documents[0]["model"], "muse-wonder-1.0");
    assert!(documents[0].get("tool_choice").is_none());
    assert_eq!(documents[0]["tools"].as_array().unwrap().len(), 2);
    assert!(documents[1].get("tool_choice").is_none());
    assert_eq!(
        documents[2]["tool_choice"],
        serde_json::json!({"type": "auto"})
    );
}

/// A Muse history holding a thinking-only assistant turn (an interrupted or
/// truncated response) reaches the child without it: the child would replay
/// its reasoning directly before the next user message, which Meta documents
/// as a 400. Thinking answered by text or a tool call is replayed as sent, as
/// is the same history on a GPT route.
#[tokio::test]
async fn muse_history_loses_only_thinking_that_nothing_answers() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let thinking = serde_json::json!({"type": "thinking", "thinking": "", "signature": "Q-PaDg"});
    let answered = serde_json::json!({"role": "assistant", "content": [
        thinking.clone(), {"type": "text", "text": "Hello!"},
    ]});
    let history = serde_json::json!([
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": [thinking.clone()]},
        {"role": "user", "content": "continue"},
        answered.clone(),
        {"role": "user", "content": "and then?"},
    ]);
    for model in ["muse-wonder-1.0", "claude-gpt-test"] {
        let body = serde_json::json!({"model": model, "max_tokens": 16, "messages": history});
        let (status, response) = send(&app, body.to_string(), &[]).await;
        assert_eq!(status, StatusCode::OK, "{model}: {response}");
    }
    let forwarded = cpa_observed.lock().await;
    let muse: serde_json::Value = serde_json::from_slice(&forwarded[0].body).unwrap();
    assert_eq!(
        muse["messages"],
        serde_json::json!([
            {"role": "user", "content": "hi"},
            {"role": "user", "content": "continue"},
            answered,
            {"role": "user", "content": "and then?"},
        ])
    );
    let gpt: serde_json::Value = serde_json::from_slice(&forwarded[1].body).unwrap();
    assert_eq!(gpt["messages"], history);
}

/// The router's text for a request refused because a login undoes one of
/// the child's model exclusions, naming `needle`.
fn assert_exclusion_refusal(status: StatusCode, body: &str, needle: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(error["error"]["type"], "invalid_request_error", "{body}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("model-router refused this request: "),
        "{message}"
    );
    assert!(message.contains(needle), "{message}");
}

/// A Meta login in a subdirectory setting the Contributor header when the
/// router starts is loaded by the child, which never re-reads it while it
/// runs: removing the header, or the login, keeps Muse requests refused
/// until a restart.
#[tokio::test]
async fn a_contributor_header_seen_at_startup_stays_refused_until_a_restart() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let nested = auth.path().join("sub/meta-b.json");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(
        &nested,
        r#"{"type":"meta","api_key":"k","headers":{"X-Tbh-Contributor-Mode":"1"}}"#,
    )
    .unwrap();
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    std::fs::write(&nested, r#"{"type":"meta","api_key":"k"}"#).unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("sub/meta-b.json"), "{body}");
    assert!(body.contains("model-router service restart"), "{body}");
    std::fs::remove_file(&nested).unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(cpa_observed.lock().await.is_empty());

    let restarted = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let (status, body) = send(&restarted, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// In Standard mode a Meta login with its own `excluded_models` list would
/// replace the child's `*-contributor` exclusion (`CLIProxyAPI` 8.0.33+), so
/// Muse requests are refused, read per request, and stay refused after the
/// fix until the router (and with it the child) restarts. GPT and Grok
/// requests do not rely on it.
#[tokio::test]
async fn standard_mode_refuses_muse_while_a_meta_login_lists_its_own_exclusions() {
    fn handler(_parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        plain_message_response()
    }
    let (cpa_address, cpa_observed) = spawn_fake(handler).await;
    let auth = muse_auth_dir();
    let login = auth.path().join("meta-a@example.com-0123456789abcdef.json");
    let app = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    std::fs::write(
        &login,
        r#"{"type":"meta","api_key":"k","excluded_models":["muse-wonder-full-1.0"]}"#,
    )
    .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_exclusion_refusal(status, &body, "\"*-contributor\"");
    for model in ["claude-gpt-test", "grok-4.5"] {
        let (status, body) = send(&app, effort_body(model), &[]).await;
        assert_eq!(status, StatusCode::OK, "{model}: {body}");
    }
    assert_eq!(cpa_observed.lock().await.len(), 2);

    std::fs::write(
        &login,
        r#"{"type":"meta","api_key":"k","excluded_models":["muse-wonder-full-1.0","*-contributor"]}"#,
    )
    .unwrap();
    let (status, body) = send(&app, effort_body("muse-wonder-1.0"), &[]).await;
    assert_exclusion_refusal(status, &body, "model-router service restart");
    assert_eq!(cpa_observed.lock().await.len(), 2);

    let restarted = model_router::proxy::app(muse_config(cpa_address, Some(auth.path())))
        .await
        .unwrap();
    let (status, body) = send(&restarted, effort_body("muse-wonder-1.0"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(cpa_observed.lock().await.len(), 3);
}

/// A managed config with no running child: a request the login gate lets
/// through ends at "supervisor failed to start" instead of reaching a child.
fn managed_config(auth_dir: &std::path::Path, routes: &[(&str, &str)]) -> Config {
    let mut config = Config {
        models: routes
            .iter()
            .map(|(routing_id, upstream_model)| ModelRoute {
                routing_id: (*routing_id).to_string(),
                upstream: "cliproxy".to_string(),
                upstream_model: (*upstream_model).to_string(),
                display_name: (*routing_id).to_string(),
                ..Default::default()
            })
            .collect(),
        ..Config::default()
    };
    config.auth_dir = Some(auth_dir.to_path_buf());
    config
}

fn assert_passed_the_gate(status: StatusCode, body: &str) {
    assert_ne!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("supervisor failed to start"), "{body}");
}

/// With `[muse]` off the child excludes every Meta model, so `auto` cannot
/// resolve to one; a Meta login with its own exclusion list would undo that,
/// so `auto` routes are refused once one exists, until a restart after the
/// fix. Named GPT routes cannot reach Meta and pass.
#[tokio::test]
async fn muse_off_refuses_auto_while_a_meta_login_lists_its_own_exclusions() {
    let auth = tempfile::tempdir().unwrap();
    let config = || managed_config(auth.path(), &[("any", "auto"), ("gpt", "gpt-test")]);
    let app = model_router::proxy::app(config()).await.unwrap();
    let (status, body) = send(&app, effort_body("any"), &[]).await;
    assert_passed_the_gate(status, &body);

    let login = auth.path().join("meta-a@example.com.json");
    std::fs::write(&login, r#"{"type":"meta","excluded-models":"other"}"#).unwrap();
    let (status, body) = send(&app, effort_body("any"), &[]).await;
    assert_exclusion_refusal(status, &body, "Meta login meta-a@example.com.json");
    let (status, body) = send(&app, effort_body("gpt"), &[]).await;
    assert_passed_the_gate(status, &body);

    std::fs::write(&login, r#"{"type":"meta","excluded-models":"other,*"}"#).unwrap();
    let (status, body) = send(&app, effort_body("any"), &[]).await;
    assert_exclusion_refusal(status, &body, "Meta login meta-a@example.com.json");
    let restarted = model_router::proxy::app(config()).await.unwrap();
    let (status, body) = send(&restarted, effort_body("any"), &[]).await;
    assert_passed_the_gate(status, &body);
}

/// In API-key mode the child excludes every model from the Codex logins so
/// GPT requests never fall back to the subscription; a Codex login with its
/// own exclusion list would undo that, so GPT routes are refused once one
/// exists. The running child keeps what it loaded from a login it can no
/// longer parse, and applies a fixed one only later, so emptying, breaking
/// or fixing the file keeps the refusal until a restart; at a restart an
/// empty or broken file is refused while it hides its login.
#[tokio::test]
async fn api_key_mode_refuses_gpt_while_a_codex_login_lists_its_own_exclusions() {
    let auth = tempfile::tempdir().unwrap();
    let config = || {
        let mut config = managed_config(auth.path(), &[("gpt", "gpt-test")]);
        config.gpt.auth = model_router::config::GptAuth::ApiKey;
        config.gpt.api_key = Some("sk-test".to_string());
        config
    };
    let app = model_router::proxy::app(config()).await.unwrap();
    let login = auth.path().join("codex-1-a@example.com-pro.json");
    std::fs::write(&login, r#"{"type":"codex","excluded_models":["gpt-5"]}"#).unwrap();
    let (status, body) = send(&app, effort_body("gpt"), &[]).await;
    assert_exclusion_refusal(status, &body, "Codex login codex-1-a@example.com-pro.json");

    for contents in [
        "",
        r#"{"type":"codex","#,
        r#"{"type":"codex","excluded_models":[]}"#,
    ] {
        std::fs::write(&login, contents).unwrap();
        let (status, body) = send(&app, effort_body("gpt"), &[]).await;
        assert_exclusion_refusal(status, &body, "model-router service restart");
    }

    for contents in ["", r#"{"type":"codex","#] {
        std::fs::write(&login, contents).unwrap();
        let restarted = model_router::proxy::app(config()).await.unwrap();
        let (status, body) = send(&restarted, effort_body("gpt"), &[]).await;
        assert_exclusion_refusal(
            status,
            &body,
            "codex-1-a@example.com-pro.json cannot be checked",
        );
    }
    std::fs::write(&login, r#"{"type":"codex","excluded_models":[]}"#).unwrap();
    let restarted = model_router::proxy::app(config()).await.unwrap();
    let (status, body) = send(&restarted, effort_body("gpt"), &[]).await;
    assert_passed_the_gate(status, &body);
}

/// The HTTPS-proxy front: a CONNECT to api.anthropic.com carrying the
/// router credential is terminated with the local CA and routed like a
/// base-URL request (no ingress token in the path); a wrong credential is
/// refused; anything else is a blind tunnel to the named host.
#[tokio::test]
async fn https_proxy_intercepts_only_credentialed_anthropic_tunnels() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn ok(_: &axum::http::request::Parts, _: &Bytes) -> Response {
        Response::new(Body::from("fake-upstream"))
    }
    let (fake_address, observed) = spawn_fake(ok).await;
    let ca_dir = tempfile::tempdir().unwrap();
    let interceptor = model_router::mitm::Interceptor::load(ca_dir.path(), "testtoken").unwrap();
    let ca_pem = std::fs::read(interceptor.ca_cert()).unwrap();
    let config = Config {
        anthropic_upstream_base: format!("http://{fake_address}"),
        upstreams: external_upstreams(format!("http://{fake_address}")),
        ingress_token: Some("testtoken".to_string()),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
            ..Default::default()
        }],
        interceptor: Some(Arc::new(interceptor)),
        ..Config::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router = listener.local_addr().unwrap();
    tokio::spawn(async move {
        model_router::proxy::serve_listener(listener, config, None, std::future::pending())
            .await
            .unwrap();
    });

    let client = |credential: &str| {
        reqwest::Client::builder()
            .proxy(
                reqwest::Proxy::https(format!("http://{router}"))
                    .unwrap()
                    .custom_http_auth(credential.parse().unwrap()),
            )
            .add_root_certificate(reqwest::Certificate::from_pem(&ca_pem).unwrap())
            .build()
            .unwrap()
    };

    // Credentialed: intercepted, and routed by model like the base-URL path.
    let trusted = client("Bearer testtoken");
    for model in ["claude-sonnet-4-5", "claude-gpt-test"] {
        let response = trusted
            .post("https://api.anthropic.com/v1/messages")
            .header("authorization", "Bearer claude-secret")
            .body(format!(r#"{{"model":"{model}","messages":[]}}"#))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{model}");
    }
    {
        let observed = observed.lock().await;
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].uri, "/v1/messages");
        assert_eq!(observed[0].headers["authorization"], "Bearer claude-secret");
        let routed: serde_json::Value = serde_json::from_slice(&observed[1].body).unwrap();
        assert_eq!(routed["model"], "gpt-test");
        assert_eq!(
            observed[1].headers["authorization"],
            "Bearer gateway-secret"
        );
    }

    // A wrong credential is refused, not tunnelled past the router.
    let error = client("Bearer stale")
        .get("https://api.anthropic.com/v1/models")
        .send()
        .await
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("ProxyAuthRequired"),
        "{error:?}"
    );

    // Without the credential (and with it, to another host): a blind tunnel.
    for credential in ["", "Proxy-Authorization: Bearer testtoken\r\n"] {
        let mut stream = tokio::net::TcpStream::connect(router).await.unwrap();
        stream
            .write_all(
                format!(
                    "CONNECT {fake_address} HTTP/1.1\r\nHost: {fake_address}\r\n{credential}\r\n\
                     GET /blind HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.ends_with("fake-upstream"), "{reply}");
    }
    {
        let observed = observed.lock().await;
        assert_eq!(observed.len(), 4);
        assert!(observed[2..].iter().all(|request| request.uri == "/blind"));
    }

    // Plain HTTP through the same proxy (absolute form, as Claude Code sends
    // an http:// MCP server's requests): forwarded, never the gateway, and
    // the proxy credential stays behind.
    let plain = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{router}"))
                .unwrap()
                .custom_http_auth("Bearer testtoken".parse().unwrap()),
        )
        .build()
        .unwrap();
    let response = plain
        .post(format!("http://{fake_address}/mcp?x=1"))
        .body("ping")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "fake-upstream");
    let observed = observed.lock().await;
    assert_eq!(observed.len(), 5);
    assert_eq!(observed[4].uri, "/mcp?x=1");
    assert_eq!(&observed[4].body[..], b"ping");
    assert!(!observed[4].headers.contains_key("proxy-authorization"));
}

/// Without a loadable CA, Claude Code's credentialed Anthropic tunnel fails
/// visibly (503) instead of skipping the router.
#[tokio::test]
async fn credentialed_anthropic_tunnels_fail_without_a_ca() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router = listener.local_addr().unwrap();
    let config = Config {
        upstreams: stub_upstreams(),
        ingress_token: Some("testtoken".to_string()),
        ..Config::default()
    };
    tokio::spawn(async move {
        model_router::proxy::serve_listener(listener, config, None, std::future::pending())
            .await
            .unwrap();
    });
    let mut stream = tokio::net::TcpStream::connect(router).await.unwrap();
    stream
        .write_all(
            b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\
              Proxy-Authorization: Bearer testtoken\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = [0; 12];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"HTTP/1.1 503");
}
