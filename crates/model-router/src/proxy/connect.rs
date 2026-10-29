//! The HTTPS-proxy front: Claude Code reaches the router through
//! `HTTPS_PROXY` instead of `ANTHROPIC_BASE_URL`, so it still believes it
//! talks to `api.anthropic.com` (Remote Control stays available, and the
//! desktop app's pinned base URL no longer bypasses the router).
//!
//! A CONNECT to the intercepted host that carries the router's credential
//! (sent only by Claude Code's `proxyAuthHelper`) is terminated with the
//! local CA's leaf ([`crate::mitm`]) and its requests served by the same
//! handler as the base-URL path. Every other CONNECT is a blind tunnel: the
//! processes Claude Code spawns inherit `HTTPS_PROXY` but neither the
//! credential nor the CA, so their traffic must pass untouched. A wrong
//! credential is refused rather than tunnelled, so a stale token fails
//! visibly instead of silently skipping the router. Plain-HTTP requests sent
//! to the proxy in absolute form are forwarded as they are ([`forward`]).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::{Extension, Router, routing::any};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpStream;

use super::AppState;
use crate::mitm::{INTERCEPTED_HOST, INTERCEPTED_PORT, Interceptor};

/// Marks a request that arrived inside an intercepted tunnel: the tunnel's
/// credential stands in for the ingress token.
#[derive(Clone, Copy, Debug)]
pub(super) struct Intercepted;

const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(super) async fn connect(state: &AppState, mut parts: axum::http::request::Parts) -> Response {
    if parts.extensions.get::<Intercepted>().is_some() {
        return plain(
            StatusCode::BAD_REQUEST,
            "CONNECT inside an intercepted tunnel",
        );
    }
    let Some(authority) = parts.uri.authority().cloned() else {
        return plain(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    let Some(port) = authority.port_u16() else {
        return plain(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    // `[::1]:443` names the host `[::1]`; the socket wants it bare.
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let Some(on_upgrade) = parts.extensions.remove::<hyper::upgrade::OnUpgrade>() else {
        return plain(StatusCode::BAD_REQUEST, "connection cannot be upgraded");
    };

    let intercepted = host.eq_ignore_ascii_case(INTERCEPTED_HOST) && port == INTERCEPTED_PORT;
    let Some(credential) = parts.headers.get(header::PROXY_AUTHORIZATION) else {
        return tunnel(&host, port, on_upgrade).await;
    };
    let Some(interceptor) = state.config.interceptor.clone() else {
        // Claude Code's own Anthropic tunnel expects interception: without a
        // CA it fails visibly instead of silently skipping the router.
        if intercepted {
            return plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "model-router: the HTTPS-proxy CA could not be loaded (see `model-router doctor`)",
            );
        }
        return tunnel(&host, port, on_upgrade).await;
    };
    match credential {
        value if !interceptor.credential_matches(value) => {
            tracing::warn!(%authority, "refused a CONNECT with a wrong proxy credential");
            let mut response = plain(
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "model-router: wrong proxy credential (is proxyAuthHelper stale?)",
            );
            response.headers_mut().insert(
                header::PROXY_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer realm=\"model-router\""),
            );
            response
        }
        _ if intercepted => {
            intercept(state.clone(), interceptor, on_upgrade);
            tracing::debug!(%authority, "intercepting tunnel");
            established()
        }
        _ => tunnel(&host, port, on_upgrade).await,
    }
}

fn intercept(
    state: AppState,
    interceptor: Arc<Interceptor>,
    on_upgrade: hyper::upgrade::OnUpgrade,
) {
    tokio::spawn(async move {
        let upgraded = match on_upgrade.await {
            Ok(upgraded) => upgraded,
            Err(error) => {
                tracing::warn!(%error, "intercepted tunnel never upgraded");
                return;
            }
        };
        let tls = match interceptor.acceptor().accept(TokioIo::new(upgraded)).await {
            Ok(tls) => tls,
            Err(error) => {
                // Most likely a client that does not trust the CA.
                tracing::warn!(%error, "TLS handshake on an intercepted tunnel failed");
                return;
            }
        };
        let app = Router::new()
            .fallback(any(super::dispatch))
            .with_state(state)
            .layer(Extension(Intercepted));
        if let Err(error) = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(tls), TowerToHyperService::new(app))
            .await
        {
            tracing::debug!(%error, "intercepted connection ended with an error");
        }
    });
}

async fn tunnel(host: &str, port: u16, on_upgrade: hyper::upgrade::OnUpgrade) -> Response {
    let upstream = match tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            tracing::debug!(%host, port, %error, "tunnel upstream connect failed");
            return plain(StatusCode::BAD_GATEWAY, "upstream connect failed");
        }
        Err(_) => return plain(StatusCode::GATEWAY_TIMEOUT, "upstream connect timed out"),
    };
    tracing::debug!(%host, port, "blind tunnel");
    let host = host.to_string();
    tokio::spawn(async move {
        let mut upstream = upstream;
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut client = TokioIo::new(upgraded);
                if let Err(error) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                {
                    tracing::debug!(%host, port, %error, "tunnel ended with an error");
                }
            }
            Err(error) => tracing::debug!(%host, port, %error, "tunnel never upgraded"),
        }
    });
    established()
}

/// A plain-HTTP request in absolute form (`GET http://host/path`): Claude
/// Code sends its `http://` URLs through the same proxy, a local MCP
/// server's or a `WebFetch` included. Forwarded as received, less
/// hop-by-hop and proxy headers; it never reaches the gateway.
pub(super) async fn forward(state: &AppState, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    if parts.uri.scheme_str() != Some("http") {
        return plain(
            StatusCode::BAD_REQUEST,
            "only http:// is forwarded in absolute form; https goes through CONNECT",
        );
    }
    tracing::debug!(method = %parts.method, uri = %parts.uri, "plain-HTTP forward");
    let headers = crate::headers::request_headers(&parts.headers, false, false, None);
    let sent = state
        .client
        .request(parts.method, parts.uri.to_string())
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await;
    match sent {
        Ok(response) => {
            let mut forwarded = Response::new(Body::empty());
            *forwarded.status_mut() = response.status();
            *forwarded.headers_mut() = crate::headers::response_headers(response.headers(), false);
            *forwarded.body_mut() = Body::from_stream(response.bytes_stream());
            forwarded
        }
        Err(error) => {
            tracing::debug!(
                uri = %parts.uri,
                error = format!("{:#}", anyhow::Error::from(error)),
                "forwarding a plain-HTTP request failed"
            );
            plain(StatusCode::BAD_GATEWAY, "upstream request failed")
        }
    }
}

fn established() -> Response {
    Response::new(Body::empty())
}

fn plain(status: StatusCode, message: &'static str) -> Response {
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = status;
    response
}
