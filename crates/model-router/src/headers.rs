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

#[derive(Clone)]
pub struct GptUpstreamCredential {
    api_key: HeaderValue,
    authorization: HeaderValue,
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
        })
    }

    /// Attaches both credential forms to a reqwest request, keeping the
    /// readiness probe and the proxy request path on one implementation.
    pub fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("x-api-key", self.api_key.clone())
            .header(header::AUTHORIZATION, self.authorization.clone())
    }
}

/// The Claude Code headers `CLIProxyAPI` reads the prompt-cache identity
/// (and reasoning-replay scope) from.
pub const SESSION_ID_HEADER: &str = "x-claude-code-session-id";
pub const AGENT_ID_HEADER: &str = "x-claude-code-agent-id";

/// A copy of `input` whose session header carries the shared-prefix
/// prompt-cache key and whose agent header is removed — the pair
/// `CLIProxyAPI` folds into the upstream `prompt_cache_key`
/// ([`crate::prompt_cache`]). `None` when the key is not a valid header
/// value.
#[must_use]
pub fn with_cache_identity(input: &HeaderMap, key: &str) -> Option<HeaderMap> {
    let value = axum::http::HeaderValue::from_str(key).ok()?;
    let mut output = input.clone();
    output.insert(SESSION_ID_HEADER, value);
    output.remove(AGENT_ID_HEADER);
    Some(output)
}

#[must_use]
pub fn request_headers(
    input: &HeaderMap,
    strip_credentials: bool,
    body_changed: bool,
    gpt_credential: Option<&GptUpstreamCredential>,
) -> HeaderMap {
    let mut output = filter(input, |name| {
        name == header::HOST
            || (body_changed && name == header::CONTENT_LENGTH)
            || (strip_credentials
                && (is_credential_header(name.as_str()) || name == "anthropic-beta"))
    });
    if strip_credentials && let Some(credential) = gpt_credential {
        output.insert("x-api-key", credential.api_key.clone());
        output.insert(header::AUTHORIZATION, credential.authorization.clone());
    }
    output
}

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
        if is_hop_by_hop(name)
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

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str())
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
        assert!(!headers.contains_key(header::HOST));
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-remove"));
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
    fn claude_branch_keeps_cookies() {
        let headers = request_headers(&sample_headers(), false, false, None);
        assert_eq!(headers[header::COOKIE], "session=abc");
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
}
