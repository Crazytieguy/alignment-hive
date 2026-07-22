use std::sync::Arc;
use std::time::{Duration, Instant};

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

    let models_response = reqwest::Client::new()
        .get(format!("http://{router_address}/v1/models?source=gateway"))
        .header("authorization", "Bearer discovery-secret")
        .send()
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
    assert_eq!(models["data"][1]["display_name"], "Existing GPT Test");

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
        assert_eq!(parts.headers["authorization"], "Bearer discovery-secret");
        let mut response = Response::new(Body::from(
            r#"{"data":[{"id":"claude-existing","display_name":"Claude Existing","type":"model"},{"id":"claude-gpt-test","display_name":"Existing GPT Test","type":"model"}]}"#,
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

#[tokio::test]
async fn models_discovery_falls_back_to_routes_for_every_upstream_failure_shape() {
    let fake_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_address = fake_listener.local_addr().unwrap();
    let fake_app = Router::new().fallback(any(|request: Request| async move {
        match request.uri().query() {
            Some("case=non200") => Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::from("unavailable"))
                .unwrap(),
            Some("case=invalid") => Response::new(Body::from("not json")),
            Some("case=missing") => Response::new(Body::from("{}")),
            Some("case=slow") => {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Response::new(Body::from(r#"{"data":[]}"#))
            }
            other => panic!("unexpected discovery query: {other:?}"),
        }
    }));
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let config = Config {
        anthropic_upstream_base: format!("http://{fake_address}"),
        upstreams: stub_upstreams(),
        ingress_token: Some("discovery-token".to_string()),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
        }],
        openai_providers: vec![model_router::config::OpenAiProvider {
            name: "fireworks".to_string(),
            base_url: "https://api.fireworks.ai/inference/v1".to_string(),
            api_key: Some("unused-in-this-test".to_string()),
            models: vec![model_router::config::ProviderModel {
                name: "accounts/fireworks/models/kimi-k2p7".to_string(),
                routing_id: "kimi-k2.7".to_string(),
                display_name: "Kimi K2.7".to_string(),
            }],
        }],
        ..Config::default()
    };
    let app = model_router::proxy::app(config).await.unwrap();

    for failure_case in ["non200", "invalid", "missing", "slow"] {
        let started = Instant::now();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/t/discovery-token/v1/models?case={failure_case}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "case {failure_case}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "case {failure_case} exceeded Claude Code's discovery timeout"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Both the configured route and the provider-derived route appear.
        assert_eq!(document["data"].as_array().unwrap().len(), 2);
        assert_eq!(document["data"][0]["id"], "claude-gpt-test");
        assert_eq!(document["data"][0]["type"], "model");
        assert_eq!(document["data"][0]["display_name"], "GPT Test");
        assert_eq!(document["data"][1]["id"], "kimi-k2.7");
        assert_eq!(document["data"][1]["display_name"], "Kimi K2.7");
    }

    let ungated = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ungated.status(), StatusCode::NOT_FOUND);
    fake_task.abort();
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

/// Spawns a single-purpose fake upstream returning `handler`'s response and
/// recording every request. Returns `None` when the sandbox forbids loopback
/// listeners.
async fn spawn_fake(
    handler: fn(&axum::http::request::Parts, &Bytes) -> Response,
) -> Option<(std::net::SocketAddr, Arc<Mutex<Vec<ObservedRequest>>>)> {
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
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: sandbox prohibits loopback listeners");
            return None;
        }
        Err(error) => panic!("failed to bind fake upstream: {error}"),
    };
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
    Some((address, observed))
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
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
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
    let Some((fake_address, observed)) = spawn_fake(handler).await else {
        return;
    };
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
    assert_eq!(document["model"], "gpt-test");
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
    let Some((fake_address, observed)) = spawn_fake(handler).await else {
        return;
    };
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
    assert!(body.contains(r#""type":"web_search_tool_result""#));
    // The empty result block was filled from the text citation, with the
    // tracking parameter stripped (the prose text block keeps its original
    // wording).
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
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
    let Some((anthropic_address, anthropic_observed)) = spawn_fake(anthropic).await else {
        return;
    };
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
    let Some((anthropic_address, anthropic_observed)) = spawn_fake(anthropic).await else {
        return;
    };
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
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
    let Some((anthropic_address, _)) = spawn_fake(anthropic).await else {
        return;
    };
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
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
    let Some((anthropic_address, anthropic_observed)) = spawn_fake(anthropic).await else {
        return;
    };
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
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

/// `off` mode disables tapping and interception on both branches.
#[tokio::test]
async fn off_mode_passes_subcalls_through_unchanged() {
    fn upstream(parts: &axum::http::request::Parts, _body: &Bytes) -> Response {
        assert_eq!(parts.uri.path(), "/v1/messages");
        Response::new(Body::from(
            r#"{"id":"msg","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
        ))
    }
    let Some((address, observed)) = spawn_fake(upstream).await else {
        return;
    };
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
    let Some((anthropic_address, anthropic_observed)) = spawn_fake(anthropic).await else {
        return;
    };
    // Default upstreams = managed mode; `app()` provides no managed handle,
    // so the GPT target is ManagedUnavailable.
    let config = Config {
        anthropic_upstream_base: format!("http://{anthropic_address}"),
        models: vec![ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
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
    let Some((anthropic_address, _)) = spawn_fake(anthropic).await else {
        return;
    };
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
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
    let Some((anthropic_address, _)) = spawn_fake(anthropic).await else {
        return;
    };
    let Some((cpa_address, cpa_observed)) = spawn_fake(cpa).await else {
        return;
    };
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
