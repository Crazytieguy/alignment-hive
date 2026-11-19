//! Whether the Gemini API key's project has billing, which decides whether a
//! Hydrogen request may go out ([`Gate`]) and what `doctor` reports
//! ([`probe`]).
//!
//! Google serves Hydrogen to a project without billing too, on a free tier
//! whose prompts it may use to improve its products, human review included.
//! Paid or free is the project's billing, not the request's: no request
//! field pins the paid tier, no API reads a key's tier, and the reply does
//! not say which one served it (Google's docs and live calls, 2026-11-18).
//! The other built-in models are paid-only, so a project without billing
//! gets an error from them where Hydrogen answers. That is the test: a
//! paid-only model answering shows the key is billed.
//!
//! The guarantee: unless the user consented to the free tier (`[gemini]
//! allow-free-tier`), a Hydrogen request is forwarded only after a [`probe`]
//! found billing on the key's project. One that does answers for the rest of
//! the gate's life, which is the router's run on that key: a billed project
//! that runs out of prepaid credit gets a 402 and one with a delinquent
//! account a 403, from Hydrogen as from the others, since Google does not
//! move those to the free tier. Only unlinking the project's billing does,
//! and a router started before that serves Hydrogen free until it restarts.
//! Whatever else a probe finds refuses the request, and answers the
//! requests after it for [`REFUSED_FOR`].
//!
//! The gate stands where a request leaves for the child
//! (`proxy::send_to_child`), not where it arrives, and goes by the model the
//! outgoing body names: a web search sub-call on a Hydrogen route can wait
//! minutes on another backend before it asks Hydrogen for anything. Gemini's
//! own search names its model in its path and asks the gate itself. The
//! auto-mode classifier's calls leave another way, but config validation
//! keeps them off Gemini routes.
//!
//! The probe's answer is a message in the body ([`is_message`]), never a
//! 200 alone: with either of its keep-alives on, the child sends its 200
//! before Google has answered, and Google's error after it. Of that error
//! the child passes on the status and the message, where the free tier's
//! quota is named, and drops the details.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;

use crate::config::{
    GEMINI_PROBE_FALLBACK_MODEL, GEMINI_PROBE_MODEL, GeminiConfig, has_gemini_free_tier,
};
use crate::headers::GptUpstreamCredential;

/// How long a probe that found no billing answers for the next requests:
/// the ones that waited on it, and a client's retries.
const REFUSED_FOR: Duration = Duration::from_secs(10);

/// The most one probe request may take: a Hydrogen request waits on it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Where to turn billing on for the key's project.
pub(crate) const BILLING_URL: &str = "https://aistudio.google.com/billing";

/// What a [`probe`] found: `Ok` when the key's project has billing.
pub(crate) type Billing = Result<(), NotPaid>;

/// What a [`probe`] found in place of billing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotPaid {
    /// Google answered from the free tier's quota: no billing.
    Free,
    /// Billed, with no prepaid credit left (402).
    Empty,
    /// Google denies the project (403), as for a past-due billing account.
    Blocked,
    /// A 429 that does not name the free tier, as a billed project past one
    /// of its limits gets, and what it said: no answer yet.
    RateLimited(String),
    /// No answer either way, and why; `retry` when asking again may get
    /// one with nothing changed (a failing server or connection, where an
    /// invalid key stays invalid).
    Unknown { why: String, retry: bool },
}

/// A probe's finding in a sentence's worth of words, for doctor and the
/// log.
pub(crate) fn summary(found: &Billing) -> String {
    match found {
        Ok(()) => "the key's project has billing".to_string(),
        Err(found) => found.summary(),
    }
}

/// What a key check straight to Google found ([`crate::set_key`]).
pub(crate) enum KeyCheck {
    /// Google rejected the key itself, and what it said.
    Invalid(String),
    /// A key Google took, and what the request showed of its billing.
    Checked(Billing),
}

/// What Google's own answer to a probe request means: as [`read`], but on
/// Google's native API, where a bad key is told apart from a bad project.
pub(crate) fn read_google(status: StatusCode, body: &[u8]) -> KeyCheck {
    if status == StatusCode::OK {
        let candidates = br#""candidates""#;
        return KeyCheck::Checked(
            if body.windows(candidates.len()).any(|at| at == candidates) {
                Ok(())
            } else {
                Err(NotPaid::Unknown {
                    why: "Google answered with no reply".to_string(),
                    retry: true,
                })
            },
        );
    }
    let invalid = [
        &b"API_KEY_INVALID"[..],
        b"API key not valid",
        b"API key expired",
    ];
    if (status == StatusCode::BAD_REQUEST || status == StatusCode::UNAUTHORIZED)
        && invalid
            .iter()
            .any(|marker| body.windows(marker.len()).any(|at| at == *marker))
    {
        return KeyCheck::Invalid(error_message(body));
    }
    KeyCheck::Checked(read(status, body))
}

/// What an answer to a probe request means.
fn read(status: StatusCode, body: &[u8]) -> Billing {
    if status == StatusCode::OK && is_message(body) {
        return Ok(());
    }
    Err(if mentions_free_tier(body) {
        NotPaid::Free
    } else if status == StatusCode::PAYMENT_REQUIRED {
        NotPaid::Empty
    } else if status == StatusCode::FORBIDDEN {
        NotPaid::Blocked
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        NotPaid::RateLimited(error_message(body))
    } else {
        let said = error_message(body);
        NotPaid::Unknown {
            why: format!("the probe was answered {status}: {said}"),
            retry: !status.is_client_error() || status == StatusCode::REQUEST_TIMEOUT,
        }
    })
}

impl NotPaid {
    /// Whether asking again may find billing with nothing changed.
    pub(crate) const fn may_pass_on_retry(&self) -> bool {
        matches!(
            self,
            Self::RateLimited(_) | Self::Unknown { retry: true, .. }
        )
    }

    /// Why a finding that is neither billing nor its absence is no answer.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::RateLimited(said) => format!("Google rate-limited the check: {said}"),
            Self::Unknown { why, .. } => why.clone(),
            Self::Free | Self::Empty | Self::Blocked => self.summary(),
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::Free => "the key's project has no billing".to_string(),
            Self::Empty => {
                "the key's project is out of prepaid credit (Google answered 402)".into()
            }
            Self::Blocked => {
                "Google denies the key's project (403), as it does for a past-due billing account \
                 or a key restricted from the Gemini API"
                    .to_string()
            }
            Self::RateLimited(said) => format!(
                "the key's billing could not be confirmed: Google rate-limited the check on \
                 {GEMINI_PROBE_MODEL} and on {GEMINI_PROBE_FALLBACK_MODEL} (429: {said})"
            ),
            Self::Unknown { why, .. } => {
                format!("the key's billing could not be confirmed ({why})")
            }
        }
    }

    /// The client-facing text for a Hydrogen request refused over this.
    pub(crate) fn refusal(&self) -> String {
        let name = "Gemini 4 Hydrogen";
        let consent = "run /model-router:setup, which explains the trade-off";
        let fix = match self {
            Self::Free => format!(
                "Google would serve it on its free tier, where it may use your prompts to \
                 improve its products. Turn billing on for the project at {BILLING_URL}, or to \
                 use {name} on the free tier, {consent}."
            ),
            Self::Empty => format!("Add credit at {BILLING_URL}."),
            Self::Blocked => format!("Check the project and its billing at {BILLING_URL}."),
            // Most likely a billed project past a limit, which the free
            // tier is no way around: the consent is not offered.
            Self::RateLimited(_) => "The request goes out once the check passes: try again \
                                     when the limit has moved on. `model-router doctor` runs \
                                     the same check."
                .to_string(),
            Self::Unknown { .. } => format!(
                "Were the project without billing, Google would serve the request on its free \
                 tier, where it may use your prompts to improve its products, so it waits for \
                 the check to pass; `model-router doctor` runs the same one. To use {name} on \
                 the free tier, {consent}."
            ),
        };
        format!(
            "model-router refused this {name} request: {}. {fix}",
            self.summary()
        )
    }
}

/// The message of an error body, or its first bytes when it has another
/// shape. Google's rate-limit messages end with how long to wait, some 350
/// characters in.
fn error_message(body: &[u8]) -> String {
    let message = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|document| Some(document.get("error")?.get("message")?.as_str()?.to_owned()))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    message.trim().chars().take(500).collect()
}

/// Whether a 200 reply from the child is a message: an error after a
/// keep-alive has none.
fn is_message(reply: &[u8]) -> bool {
    let message = br#""type":"message"#;
    reply.windows(message.len()).any(|at| at == message)
}

/// Whether a Gemini error names the free tier: its quotas
/// (`generate_content_free_tier_requests`, `...PerModel-FreeTier`) or its
/// verification (`FREE_TIER_VERIFICATION_REQUIRED`, "the Gemini API Free
/// tier"). Only a project without billing is counted against those.
fn mentions_free_tier(body: &[u8]) -> bool {
    let body = body.to_ascii_lowercase();
    [&b"free_tier"[..], b"freetier", b"free tier"]
        .iter()
        .any(|name| body.windows(name.len()).any(|window| window == *name))
}

/// Asks a paid-only model for a few tokens, through the `CLIProxyAPI` at
/// `base_url` and so on the same key as every Gemini route:
/// [`GEMINI_PROBE_MODEL`], and where Google rate-limits that one (its
/// limits are per model) [`GEMINI_PROBE_FALLBACK_MODEL`]. Billed to the key
/// when it answers: about $0.0005 (some 15 tokens in and 20 out, thinking
/// included, on 2026-11-18).
pub(crate) async fn probe(
    client: &reqwest::Client,
    base_url: &str,
    credential: Option<&GptUpstreamCredential>,
) -> Billing {
    let found = ask(client, base_url, credential, GEMINI_PROBE_MODEL).await;
    if matches!(found, Err(NotPaid::RateLimited(_))) {
        return ask(client, base_url, credential, GEMINI_PROBE_FALLBACK_MODEL).await;
    }
    found
}

/// One probe request, to `model` at its lowest effort. It names no
/// `max_tokens`: the child asks for none and forwards none.
async fn ask(
    client: &reqwest::Client,
    base_url: &str,
    credential: Option<&GptUpstreamCredential>,
    model: &str,
) -> Billing {
    let body = serde_json::json!({
        "model": format!("{model}(low)"),
        "messages": [{"role": "user", "content": "Reply with the word ok."}],
    });
    let request = client
        .post(format!("{}/v1/messages", base_url.trim_end_matches('/')))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .timeout(PROBE_TIMEOUT)
        .body(body.to_string());
    let request = match credential {
        Some(credential) => credential.apply(request),
        None => request,
    };
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            match response.bytes().await {
                Ok(body) => read(status, &body),
                Err(error) => Err(NotPaid::Unknown {
                    why: format!("the answer was cut off: {error}"),
                    retry: true,
                }),
            }
        }
        Err(error) => Err(NotPaid::Unknown {
            why: format!(
                "CLIProxyAPI could not be asked: {:#}",
                anyhow::Error::from(error.without_url())
            ),
            retry: true,
        }),
    }
}

/// What Hydrogen requests pass unless the user consented to the free tier:
/// a probe's finding of billing for good, any other for [`REFUSED_FOR`].
pub(crate) struct Gate {
    /// Whether anything is gated: not with the family off, nor with the
    /// user's consent (`[gemini] allow-free-tier`).
    on: bool,
    /// What the last probe found, and until when that answers (`None`:
    /// for good). Held across a probe, so requests that arrive together
    /// wait on one.
    last: tokio::sync::Mutex<Option<(Option<Instant>, Billing)>>,
}

impl Gate {
    pub(crate) fn new(config: &GeminiConfig) -> Arc<Self> {
        Arc::new(Self {
            on: config.enabled && !config.allow_free_tier,
            last: tokio::sync::Mutex::new(None),
        })
    }

    /// Whether a request about to go to the child with `body` needs
    /// [`Self::admit`] first: one whose model is Hydrogen, at any effort.
    pub(crate) fn gates(&self, body: &[u8]) -> bool {
        crate::routing::find_top_level_value_range(body, "model")
            .and_then(|range| serde_json::from_slice::<String>(&body[range]).ok())
            .is_some_and(|model| self.gates_model(&model))
    }

    /// Whether a request for `model` (a native Gemini request names it in
    /// its path) needs [`Self::admit`] first.
    pub(crate) fn gates_model(&self, model: &str) -> bool {
        self.on && has_gemini_free_tier(model)
    }

    /// Lets a gated request through on a probe that found billing: the last
    /// one while it answers, or else `probe`, run now. `Err` is what
    /// refuses the request.
    pub(crate) async fn admit(&self, probe: impl Future<Output = Billing>) -> Billing {
        let mut last = self.last.lock().await;
        if let Some((until, found)) = last.as_ref()
            && until.is_none_or(|until| Instant::now() < until)
        {
            return found.clone();
        }
        let found = probe.await;
        tracing::info!(found = %summary(&found), "probed the Gemini key's billing");
        let until = found.is_err().then(|| Instant::now() + REFUSED_FOR);
        *last = Some((until, found.clone()));
        found
    }

    /// Ends the time a refusing probe answers for, as the clock would.
    #[cfg(test)]
    pub(crate) fn expire(&self) {
        let mut last = self.last.try_lock().expect("no probe is running");
        last.as_mut().expect("a probe ran").0 = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 429 in the shape of Google's quota errors, with the metric its
    /// forum reports for a paid-only model on a project without billing
    /// (`generate_content_free_tier_requests, limit: 0`). There was no such
    /// key to capture one from.
    const FREE_TIER_429: &str = r#"{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, head to: https://ai.google.dev/gemini-api/docs/rate-limits.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-4-neon","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]}]}}"#;

    #[test]
    fn an_answer_to_the_probe_is_read_by_status_and_free_tier_names() {
        let read =
            |status: u16, body: &str| read(StatusCode::from_u16(status).unwrap(), body.as_bytes());
        let unknown = |why: &str, retry| {
            Err(NotPaid::Unknown {
                why: why.to_string(),
                retry,
            })
        };
        assert_eq!(
            read(200, r#"{"id":"","type":"message","role":"assistant"}"#),
            Ok(())
        );
        assert_eq!(read(429, FREE_TIER_429), Err(NotPaid::Free));
        // The child's keep-alive, on, sends a 200 ahead of Google's answer.
        assert_eq!(
            read(200, &format!("\n\n{FREE_TIER_429}")),
            Err(NotPaid::Free)
        );
        assert_eq!(
            read(200, "\n{\"type\":\"error\",\"error\":{\"message\":\"x\"}}"),
            unknown("the probe was answered 200 OK: x", true)
        );
        // An unverified project without billing.
        assert_eq!(
            read(
                403,
                r#"{"error":{"message":"FREE_TIER_VERIFICATION_REQUIRED"}}"#
            ),
            Err(NotPaid::Free)
        );
        assert_eq!(
            read(
                403,
                "Project 1 must be verified before it can use the Gemini API Free tier."
            ),
            Err(NotPaid::Free)
        );
        assert_eq!(
            read(402, r#"{"error":{"message":"no credit"}}"#),
            Err(NotPaid::Empty)
        );
        assert_eq!(
            read(
                403,
                r#"{"error":{"message":"Your project has been denied access"}}"#
            ),
            Err(NotPaid::Blocked)
        );
        // A paid project past its rate limit, or Google failing: no answer
        // yet. An invalid key: none until the key changes.
        let limited = read(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Resource has been exhausted"}}"#,
        );
        assert_eq!(
            limited,
            Err(NotPaid::RateLimited(
                "Resource has been exhausted".to_string()
            ))
        );
        assert!(limited.unwrap_err().may_pass_on_retry());
        let failing = read(503, "overloaded");
        assert_eq!(
            failing,
            unknown(
                "the probe was answered 503 Service Unavailable: overloaded",
                true
            )
        );
        assert!(failing.unwrap_err().may_pass_on_retry());
        let invalid = read(400, r#"{"error":{"message":"API key not valid."}}"#);
        assert_eq!(
            invalid,
            unknown(
                "the probe was answered 400 Bad Request: API key not valid.",
                false
            )
        );
        assert!(!invalid.unwrap_err().may_pass_on_retry());
        for settled in [NotPaid::Free, NotPaid::Empty, NotPaid::Blocked] {
            assert!(!settled.may_pass_on_retry());
        }
    }

    #[test]
    fn google_s_own_answer_tells_a_bad_key_from_a_bad_project() {
        let invalid = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"}]}}"#;
        assert!(matches!(
            read_google(StatusCode::BAD_REQUEST, invalid.as_bytes()),
            KeyCheck::Invalid(said) if said == "API key not valid. Please pass a valid API key."
        ));
        let reply = br#"{"candidates":[{"content":{"parts":[{"text":"ok"}],"role":"model"}}]}"#;
        assert!(matches!(
            read_google(StatusCode::OK, reply),
            KeyCheck::Checked(Ok(()))
        ));
        assert!(matches!(
            read_google(StatusCode::TOO_MANY_REQUESTS, FREE_TIER_429.as_bytes()),
            KeyCheck::Checked(Err(NotPaid::Free))
        ));
        assert!(matches!(
            read_google(StatusCode::PAYMENT_REQUIRED, b"{}"),
            KeyCheck::Checked(Err(NotPaid::Empty))
        ));
        assert!(matches!(
            read_google(StatusCode::SERVICE_UNAVAILABLE, b"{}"),
            KeyCheck::Checked(Err(NotPaid::Unknown { retry: true, .. }))
        ));
    }

    #[test]
    fn only_free_tier_names_count_as_a_mention() {
        for body in [
            FREE_TIER_429,
            "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
            "FREE_TIER_VERIFICATION_REQUIRED",
            "the Gemini API Free tier",
        ] {
            assert!(mentions_free_tier(body.as_bytes()), "{body}");
        }
        for body in [
            "",
            "Quota exceeded for metric: generate_content_paid_tier_input_token_count",
            "You exceeded your current quota, please check your plan and billing details.",
            "free",
        ] {
            assert!(!mentions_free_tier(body.as_bytes()), "{body}");
        }
    }

    /// Only the free tier's own refusal offers the consent to it, with the
    /// probe that could not tell; a rate limit's does not.
    #[test]
    fn a_refusal_offers_the_free_tier_only_where_it_may_be_the_way_on() {
        let unknown = NotPaid::Unknown {
            why: "x".to_string(),
            retry: true,
        };
        let limited = NotPaid::RateLimited("Resource has been exhausted".to_string());
        for (found, offered) in [
            (&NotPaid::Free, true),
            (&unknown, true),
            (&NotPaid::Empty, false),
            (&NotPaid::Blocked, false),
            (&limited, false),
        ] {
            let refusal = found.refusal();
            assert!(
                refusal.starts_with("model-router refused this Gemini 4 Hydrogen request: "),
                "{refusal}"
            );
            assert_eq!(
                refusal.contains("/model-router:setup"),
                offered,
                "{refusal}"
            );
            assert_eq!(refusal.contains("free tier"), offered, "{refusal}");
        }
        let refusal = limited.refusal();
        for part in [
            "the key's billing could not be confirmed",
            "gemini-4-neon and on gemini-4-argon (429: Resource has been exhausted)",
        ] {
            assert!(refusal.contains(part), "{part}: {refusal}");
        }
    }
}
