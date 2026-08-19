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
        .timeout(std::time::Duration::from_mins(1))
        .build()
        .expect("reqwest client")
}

/// Send a provider-API request, retrying on 429 with backoff (2/4/8/16s).
/// Providers rate-limit aggressively (vast: ~1 req/s per endpoint) and a 429
/// means the request was rejected before processing, so retrying is safe for
/// every verb — including instance creation. Rate limiting must surface as a
/// delay, never as an error that a caller could mistake for a dead machine.
///
/// The whole ladder is bounded by [`TOTAL_BACKOFF_BUDGET`]: waiting only pays
/// while the caller is still there, and a 429 handed back is a normal,
/// classifiable outcome for every caller here.
pub(crate) async fn send_429_retry(
    req: reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    use anyhow::Context as _;
    let mut delay = std::time::Duration::from_secs(2);
    let mut spent = std::time::Duration::ZERO;
    loop {
        let resp = req
            .try_clone()
            .context("internal: unclonable API request")?
            .send()
            .await?;
        if resp.status().as_u16() != 429 || delay.as_secs() > 16 {
            return Ok(resp);
        }
        // RunPod v2 says how long to wait (integer seconds); vast never sends
        // the header, so it keeps the ladder by construction.
        let Some(wait) = next_backoff(resp.headers(), delay, spent) else {
            tracing::debug!("provider API rate limit (429); backoff budget spent");
            return Ok(resp);
        };
        tracing::debug!(?wait, "provider API rate limit (429); backing off");
        tokio::time::sleep(wait).await;
        spent += wait;
        delay *= 2;
    }
}

/// Total time one request may spend waiting out 429s. The bare 2/4/8/16
/// ladder sums to 30 s; honoring a provider's `Retry-After` (`RunPod` v2
/// sends it) could otherwise stretch the same four steps to four minutes
/// inside a single tool call.
const TOTAL_BACKOFF_BUDGET: std::time::Duration = std::time::Duration::from_mins(1);

/// The next 429 wait, clamped to what is left of [`TOTAL_BACKOFF_BUDGET`];
/// `None` once the budget is spent, which ends the ladder.
fn next_backoff(
    headers: &reqwest::header::HeaderMap,
    fallback: std::time::Duration,
    spent: std::time::Duration,
) -> Option<std::time::Duration> {
    let wait = retry_after_delay(headers, fallback).min(TOTAL_BACKOFF_BUDGET.saturating_sub(spent));
    (!wait.is_zero()).then_some(wait)
}

/// How long to wait after a 429, honoring the provider's `Retry-After`
/// (`RunPod` v2 sends integer seconds). Absent, zero, or unparseable → the
/// caller's own backoff; clamped to a ceiling so a pathological header can
/// never park a tool call for an hour.
pub(crate) fn retry_after_delay(
    headers: &reqwest::header::HeaderMap,
    fallback: std::time::Duration,
) -> std::time::Duration {
    /// A provider asking for more than a minute is asking for more than a
    /// tool call can wait; the caller's own bound takes over from there.
    const CEILING: std::time::Duration = std::time::Duration::from_mins(1);

    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        // 0 (or an HTTP-date, which does not parse as an integer) must never
        // turn the retry loop into a busy loop.
        .filter(|secs| *secs > 0)
        .map_or(fallback, |secs| {
            std::time::Duration::from_secs(secs).min(CEILING)
        })
}

pub mod config;
pub mod descriptions;
pub mod heartbeat;
pub mod jupyter;
pub mod ledger;
pub mod machine_scripts;
pub mod notebook;
pub mod runpod;
pub mod runtime;
pub mod server;
pub mod ssh;
pub mod ssh_exec;
pub mod state;
pub mod sync;
pub mod ulid;
pub mod vast;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn retry_after_delay_parses_and_clamps() {
        let fallback = Duration::from_secs(4);
        assert_eq!(
            super::retry_after_delay(&headers("12"), fallback),
            Duration::from_secs(12)
        );
        assert_eq!(
            super::retry_after_delay(&HeaderMap::new(), fallback),
            fallback
        );
        // 0 would spin the loop with no delay at all.
        assert_eq!(super::retry_after_delay(&headers("0"), fallback), fallback);
        // A pathological value must not park a tool call for a day.
        assert_eq!(
            super::retry_after_delay(&headers("100000"), fallback),
            Duration::from_mins(1)
        );
        // HTTP-date form and outright garbage both fall back.
        assert_eq!(
            super::retry_after_delay(&headers("Wed, 21 Oct 2026 07:28:00 GMT"), fallback),
            fallback
        );
        assert_eq!(
            super::retry_after_delay(&headers("soon"), fallback),
            fallback
        );
    }

    /// Honoring `Retry-After` must not turn a bounded ladder into a
    /// four-minute stall: all four steps share one cumulative budget.
    #[test]
    fn backoff_ladder_is_bounded_in_total() {
        // The bare ladder (no header) is unchanged — it already fits.
        let mut spent = Duration::ZERO;
        for step in [2, 4, 8, 16] {
            let wait = super::next_backoff(&HeaderMap::new(), Duration::from_secs(step), spent)
                .expect("the plain ladder always fits the budget");
            assert_eq!(wait, Duration::from_secs(step));
            spent += wait;
        }
        assert_eq!(spent, Duration::from_secs(30));

        // A provider asking for the per-wait ceiling every time gets it once
        // and then the ladder ends, instead of stacking four of them.
        let mut spent = Duration::ZERO;
        let mut waits = Vec::new();
        while let Some(wait) = super::next_backoff(&headers("60"), Duration::from_secs(2), spent) {
            spent += wait;
            waits.push(wait);
            assert!(waits.len() <= 4, "the ladder must terminate: {waits:?}");
        }
        assert_eq!(waits, vec![Duration::from_mins(1)]);
        assert!(spent <= super::TOTAL_BACKOFF_BUDGET);
    }
}
