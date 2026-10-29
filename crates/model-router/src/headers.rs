//! Header policy at the router's boundaries: hop-by-hop headers are never
//! forwarded, and GPT-bound requests lose every Claude credential (and
//! `anthropic-beta`), carry the local gateway credential instead when one is
//! configured, and get the shared prompt-cache identity.

use std::collections::HashSet;

use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Credential-bearing headers. The single source of truth shared by the
/// GPT-branch strip below and capture redaction — the two security
/// boundaries must never disagree on what counts as a credential.
const CREDENTIAL_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "set-cookie",
];

/// Whether `name` (lowercase, as `HeaderName` guarantees) carries a credential.
#[must_use]
pub fn is_credential_header(name: &str) -> bool {
    CREDENTIAL_HEADERS.contains(&name)
}

/// What every request to the `CLIProxyAPI` upstream carries: the local
/// gateway secret, in both the `x-api-key` and bearer forms, each marked
/// sensitive, and, in Muse Contributor mode, the trigger for the Contributor
/// header relay ([`crate::muse::CONTRIBUTOR_TRIGGER`]).
#[derive(Clone)]
pub struct GptUpstreamCredential {
    api_key: HeaderValue,
    authorization: HeaderValue,
    muse_contributor: bool,
}

impl GptUpstreamCredential {
    /// Builds sensitive header values for a local GPT upstream.
    ///
    /// # Errors
    /// Returns an error if the key cannot be represented in an HTTP header.
    pub fn new(api_key: &str) -> Result<Self, axum::http::header::InvalidHeaderValue> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))?;
        let mut api_key = HeaderValue::from_str(api_key)?;
        api_key.set_sensitive(true);
        authorization.set_sensitive(true);
        Ok(Self {
            api_key,
            authorization,
            muse_contributor: false,
        })
    }

    /// The same credential, adding [`crate::muse::CONTRIBUTOR_TRIGGER`] to
    /// every request when `mode` is Contributor.
    #[must_use]
    pub fn for_muse_mode(self, mode: crate::config::MuseMode) -> Self {
        Self {
            muse_contributor: mode == crate::config::MuseMode::Contributor,
            ..self
        }
    }

    /// Attaches both credential forms, and the Contributor trigger when on:
    /// the one implementation behind the forwarded request headers and every
    /// reqwest side call.
    pub fn insert_into(&self, headers: &mut HeaderMap) {
        headers.insert("x-api-key", self.api_key.clone());
        headers.insert(header::AUTHORIZATION, self.authorization.clone());
        if self.muse_contributor {
            headers.insert(
                crate::muse::CONTRIBUTOR_TRIGGER,
                HeaderValue::from_static("1"),
            );
        }
    }

    /// [`Self::insert_into`] for a reqwest side call.
    pub fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut headers = HeaderMap::new();
        self.insert_into(&mut headers);
        builder.headers(headers)
    }
}

/// The Claude Code headers `CLIProxyAPI` reads the prompt-cache identity
/// (and reasoning-replay scope) from.
pub(crate) const SESSION_ID_HEADER: &str = "x-claude-code-session-id";
pub(crate) const AGENT_ID_HEADER: &str = "x-claude-code-agent-id";

/// A copy of `input` whose session header carries the shared-prefix
/// prompt-cache key and whose agent header is removed — the pair
/// `CLIProxyAPI` folds into the upstream `prompt_cache_key`
/// ([`crate::prompt_cache`]).
#[must_use]
pub fn with_cache_identity(input: &HeaderMap, key: &str) -> HeaderMap {
    let value = HeaderValue::from_str(key).expect("prefix keys are visible ASCII");
    let mut output = input.clone();
    output.insert(SESSION_ID_HEADER, value);
    output.remove(AGENT_ID_HEADER);
    output
}

/// The inbound headers to forward upstream: hop-by-hop headers (and those
/// `Connection` names) and `Host` dropped, and `Content-Length` too when the
/// body was rewritten. With `strip_credentials` (the GPT branch), every
/// credential header and `anthropic-beta` are dropped as well, and
/// `gpt_credential`, if any, is attached.
#[must_use]
pub fn request_headers(
    input: &HeaderMap,
    strip_credentials: bool,
    body_changed: bool,
    gpt_credential: Option<&GptUpstreamCredential>,
) -> HeaderMap {
    // A client's Contributor trigger never passes: only the credential
    // decides it.
    let mut output = filter(input, |name| {
        name == header::HOST
            || (body_changed && name == header::CONTENT_LENGTH)
            || (strip_credentials
                && (is_credential_header(name.as_str())
                    || name == "anthropic-beta"
                    || name == crate::muse::CONTRIBUTOR_TRIGGER))
    });
    if strip_credentials && let Some(credential) = gpt_credential {
        credential.insert_into(&mut output);
    }
    output
}

/// The upstream response headers to pass on: hop-by-hop headers (and those
/// `Connection` names) dropped, and `Content-Length` and `Content-Encoding`
/// too when the body was rewritten.
#[must_use]
pub fn response_headers(input: &HeaderMap, body_changed: bool) -> HeaderMap {
    filter(input, |name| {
        body_changed && (name == header::CONTENT_LENGTH || name == header::CONTENT_ENCODING)
    })
}

fn filter(input: &HeaderMap, additionally_remove: impl Fn(&HeaderName) -> bool) -> HeaderMap {
    let connection_tokens = connection_tokens(input);
    let mut output = HeaderMap::with_capacity(input.len());
    for (name, value) in input {
        if HOP_BY_HOP.contains(&name.as_str())
            || connection_tokens.contains(name.as_str())
            || additionally_remove(name)
        {
            continue;
        }
        output.append(name, value.clone());
    }
    output
}

fn connection_tokens(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn sample_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert("x-api-key", HeaderValue::from_static("secret-key"));
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20"),
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-remove"),
        );
        headers.insert("x-remove", HeaderValue::from_static("yes"));
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8787"));
        headers.insert(header::COOKIE, HeaderValue::from_static("session=abc"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("12"));
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        headers
    }

    #[test]
    fn claude_keeps_credentials_and_anthropic_headers() {
        let injected = GptUpstreamCredential::new("local-gateway-secret").unwrap();
        let headers = request_headers(&sample_headers(), false, false, Some(&injected));
        assert_eq!(headers[header::AUTHORIZATION], "Bearer secret");
        assert_eq!(headers["x-api-key"], "secret-key");
        assert_eq!(headers["anthropic-beta"], "oauth-2025-04-20");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert_eq!(headers[header::COOKIE], "session=abc");
        assert!(!headers.contains_key(header::HOST));
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-remove"));
    }

    #[test]
    fn a_rewritten_body_drops_the_stale_length_and_encoding_headers() {
        assert_eq!(
            request_headers(&sample_headers(), false, false, None)[header::CONTENT_LENGTH],
            "12"
        );
        assert!(
            !request_headers(&sample_headers(), false, true, None)
                .contains_key(header::CONTENT_LENGTH)
        );
        let untouched = response_headers(&sample_headers(), false);
        assert_eq!(untouched[header::CONTENT_LENGTH], "12");
        assert_eq!(untouched[header::CONTENT_ENCODING], "gzip");
        let rewritten = response_headers(&sample_headers(), true);
        assert!(!rewritten.contains_key(header::CONTENT_LENGTH));
        assert!(!rewritten.contains_key(header::CONTENT_ENCODING));
    }

    #[test]
    fn gpt_without_configured_credential_strips_credentials_and_beta() {
        let headers = request_headers(&sample_headers(), true, true, None);
        assert!(!headers.contains_key(header::AUTHORIZATION));
        assert!(!headers.contains_key("x-api-key"));
        assert!(!headers.contains_key("anthropic-beta"));
        assert!(!headers.contains_key(header::COOKIE));
        assert_eq!(headers["anthropic-version"], "2023-06-01");
    }

    #[test]
    fn gpt_attaches_both_configured_credential_forms_after_stripping() {
        let credential = GptUpstreamCredential::new("local-gateway-secret").unwrap();
        let headers = request_headers(&sample_headers(), true, true, Some(&credential));
        assert_eq!(
            headers[header::AUTHORIZATION],
            "Bearer local-gateway-secret"
        );
        assert_eq!(headers["x-api-key"], "local-gateway-secret");
        assert!(!headers.contains_key("anthropic-beta"));
        assert_eq!(headers["anthropic-version"], "2023-06-01");
    }

    #[test]
    fn only_a_contributor_mode_credential_carries_the_contributor_trigger() {
        use crate::config::MuseMode;
        use crate::muse::CONTRIBUTOR_TRIGGER;
        let mut inbound = sample_headers();
        inbound.insert(CONTRIBUTOR_TRIGGER, HeaderValue::from_static("1"));
        for mode in [MuseMode::Off, MuseMode::Standard] {
            let credential = GptUpstreamCredential::new("secret")
                .unwrap()
                .for_muse_mode(mode);
            // A client's trigger is stripped on the way to the child...
            let headers = request_headers(&inbound, true, true, Some(&credential));
            assert!(!headers.contains_key(CONTRIBUTOR_TRIGGER), "{mode:?}");
            let headers = request_headers(&inbound, true, true, None);
            assert!(!headers.contains_key(CONTRIBUTOR_TRIGGER), "{mode:?}");
            // ...and side calls carry none.
            let mut side_call = HeaderMap::new();
            credential.insert_into(&mut side_call);
            assert!(!side_call.contains_key(CONTRIBUTOR_TRIGGER), "{mode:?}");
        }
        let credential = GptUpstreamCredential::new("secret")
            .unwrap()
            .for_muse_mode(MuseMode::Contributor);
        let headers = request_headers(&sample_headers(), true, true, Some(&credential));
        assert_eq!(headers[CONTRIBUTOR_TRIGGER], "1");
        let mut side_call = HeaderMap::new();
        credential.insert_into(&mut side_call);
        assert_eq!(side_call[CONTRIBUTOR_TRIGGER], "1");
        // The Claude branch forwards to Anthropic as it came.
        let headers = request_headers(&inbound, false, false, None);
        assert_eq!(headers[CONTRIBUTOR_TRIGGER], "1");
    }
}
