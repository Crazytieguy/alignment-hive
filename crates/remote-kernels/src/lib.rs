#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]

/// Install the process-wide rustls crypto provider. kube and tungstenite link
/// rustls with different provider features (aws-lc-rs vs ring), and rustls
/// requires an explicit choice when both are present. Idempotent.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// HTTP client for provider/Jupyter REST APIs. All calls are quick
/// request/response — a client-level timeout keeps a half-dead connection
/// from hanging a tool call (and its machine cleanup) forever.
pub(crate) fn api_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("reqwest client")
}

/// Send a provider-API request, retrying on 429 with backoff (2/4/8/16s).
/// Providers rate-limit aggressively (vast: ~1 req/s per endpoint) and a 429
/// means the request was rejected before processing, so retrying is safe for
/// every verb — including instance creation. Rate limiting must surface as a
/// delay, never as an error that a caller could mistake for a dead machine.
pub(crate) async fn send_429_retry(
    req: reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    use anyhow::Context as _;
    let mut delay = std::time::Duration::from_secs(2);
    loop {
        let resp = req
            .try_clone()
            .context("internal: unclonable API request")?
            .send()
            .await?;
        if resp.status().as_u16() != 429 || delay.as_secs() > 16 {
            return Ok(resp);
        }
        tracing::debug!(?delay, "provider API rate limit (429); backing off");
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
}

pub mod config;
pub mod descriptions;
pub mod heartbeat;
pub mod jupyter;
pub mod notebook;
pub mod runpod;
pub mod runtime;
pub mod server;
pub mod ssh;
pub mod ssh_exec;
pub mod state;
pub mod sync;
pub mod vast;
