use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Router, routing::any};
use futures_util::StreamExt;
use model_router::config::{CaptureConfig, Config, ModelRoute, UpstreamConfig, UpstreamMode};
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
    let fake_listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: sandbox prohibits loopback listeners");
            return;
        }
        Err(error) => panic!("failed to bind fake upstream: {error}"),
    };
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
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
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

    let models_response = reqwest::get(format!("http://{router_address}/v1/models?source=gateway"))
        .await
        .unwrap();
    let models: serde_json::Value =
        serde_json::from_slice(&models_response.bytes().await.unwrap()).unwrap();
    let ids: Vec<_> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["claude-existing", "claude-gpt-test"]);
    assert_eq!(models["data"][1]["display_name"], "GPT Test");

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
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
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
        "codex".to_string(),
        UpstreamConfig {
            mode: UpstreamMode::Stub,
            ..UpstreamConfig::default()
        },
    )])
}

fn external_upstreams(base_url: String) -> std::collections::BTreeMap<String, UpstreamConfig> {
    std::collections::BTreeMap::from([(
        "codex".to_string(),
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
    if parts.uri.path() == "/v1/models" {
        assert_eq!(parts.method, "GET");
        assert_eq!(parts.uri.query(), Some("source=gateway"));
        let mut response = Response::new(Body::from(
            r#"{"data":[{"id":"claude-existing","display_name":"Claude Existing","type":"model"}]}"#,
        ));
        response
            .headers_mut()
            .insert("content-type", "application/json".parse().unwrap());
        return response;
    }
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
    let fake_listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: sandbox prohibits loopback listeners");
            return;
        }
        Err(error) => panic!("failed to bind fake upstream: {error}"),
    };
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
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app_with(config, None).await.unwrap();

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
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
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

    // Health: reachable bare (for the hook) and under the prefix.
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
