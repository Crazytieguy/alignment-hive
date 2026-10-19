//! The vendor-native search backends for Grok and Muse origins: xAI's or
//! Meta's hosted `web_search` on `/v1/responses`, with the source URLs
//! harvested from the streamed `web_search_call` item (both vendors stream
//! the same shape). The sub-call's domain filters are enforced here, on every
//! harvested URL, because xAI's request-side filter only steers the search
//! and Meta's search has none.

use serde_json::{Value, json};

use super::{Link, Links, Subcall};

/// xAI caps a hosted `web_search` allow-list; beyond this the filter is left
/// off the request and enforced locally instead (a rejected field would turn a
/// working search into a failed one).
pub(super) const MAX_REQUEST_ALLOWED_DOMAINS: usize = 5;

/// The domain constraints a harvested URL must satisfy, taken from the
/// sub-call's `allowed_domains` / `blocked_domains`.
///
/// This is the authoritative filter: xAI's request-side
/// `filters.allowed_domains` only steers the upstream search, Meta has no
/// such filter, and blocked domains are never sent at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomainPolicy {
    pub(super) allowed: Vec<String>,
    pub(super) blocked: Vec<String>,
}

/// Canonical form of a domain rule or a URL host: IDNA/punycode, lowercase,
/// no trailing root dot. `None` for anything that is not a bare hostname.
///
/// Rules and hosts go through the same function on purpose — comparing a
/// human-typed rule against a parsed host is only sound if both are in the
/// same form. Parsing through a URL is what applies IDNA, so `рф.example` and
/// its punycode spelling canonicalize alike.
fn canonical_domain(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_start_matches('.');
    // A rule is a bare hostname: no scheme, path, userinfo, port, or query.
    // Backslash matters as much as slash — for special schemes the URL parser
    // treats `\` as a path separator, so `example.com\evil` would otherwise
    // canonicalize to `example.com` and turn a malformed restriction into
    // whole-domain permission. Control characters matter too: the parser
    // strips tabs and newlines outright, so `exa<TAB>mple.com` would parse as
    // `example.com`.
    if domain.is_empty()
        || domain.contains(['/', '\\', '@', ':', '?', '#'])
        || domain
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return None;
    }
    let url = reqwest::Url::parse(&format!("https://{domain}/")).ok()?;
    // The rule must have been exactly a host, not something the parser
    // rearranged into other components.
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let host = host.strip_suffix('.').unwrap_or(host).to_lowercase();
    (!host.is_empty() && host != ".").then_some(host)
}

/// A domain rule the sub-call asked for that cannot be honored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidDomainRule(pub String);

impl std::fmt::Display for InvalidDomainRule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "unusable domain filter {:?}", self.0)
    }
}

impl DomainPolicy {
    /// The policy this sub-call asks for.
    ///
    /// # Errors
    /// Returns the offending rule when any filter cannot be canonicalized. A
    /// filter that cannot be honored must fail the search: silently dropping
    /// a malformed allow-list would widen it to allow-all, and dropping a
    /// malformed block-list would admit what the caller excluded.
    pub fn from_subcall(subcall: &Subcall) -> Result<Self, InvalidDomainRule> {
        fn normalize(domains: Option<&Vec<String>>) -> Result<Vec<String>, InvalidDomainRule> {
            domains
                .into_iter()
                .flatten()
                .map(|domain| {
                    canonical_domain(domain).ok_or_else(|| InvalidDomainRule(domain.clone()))
                })
                .collect()
        }
        Ok(Self {
            allowed: normalize(subcall.allowed_domains.as_ref())?,
            blocked: normalize(subcall.blocked_domains.as_ref())?,
        })
    }

    /// The allow-list to put on the request, when it is small enough to be
    /// accepted; `None` leaves filtering entirely to [`Self::admits`].
    #[must_use]
    pub fn request_filter(&self) -> Option<&[String]> {
        (!self.allowed.is_empty() && self.allowed.len() <= MAX_REQUEST_ALLOWED_DOMAINS)
            .then_some(self.allowed.as_slice())
    }

    /// Whether a harvested URL may be shown.
    ///
    /// Host-based, never substring-based: the URL is parsed and only its
    /// canonical host is compared, so `https://allowed.example@evil.example/`,
    /// `https://allowed.example.evil.example/` and `https://evil.example./`
    /// are all rejected for an `allowed.example` allow-list. Comparison is by
    /// whole label. Blocked wins over allowed.
    #[must_use]
    pub fn admits(&self, url: &str) -> bool {
        let Ok(parsed) = reqwest::Url::parse(url) else {
            return false;
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = parsed.host_str().and_then(canonical_domain) else {
            return false;
        };
        // Whole-label suffix match without building a string per rule.
        let matches = |domain: &String| {
            host == *domain
                || host
                    .strip_suffix(domain.as_str())
                    .is_some_and(|parent| parent.ends_with('.'))
        };
        if self.blocked.iter().any(matches) {
            return false;
        }
        self.allowed.is_empty() || self.allowed.iter().any(matches)
    }
}

/// Builds the xAI Responses body that runs one hosted `web_search`.
///
/// Mirrors the shape the official grok-build CLI's fallback search client
/// sends, plus the streaming fields the harvest depends on. `tool_choice`
/// is `required` so a missing `web_search_call` means the search failed
/// rather than that the model chose not to search — verified against a child
/// whose advertised hosted-tool set is exactly `web_search`.
pub(super) fn xai_search_request_body(
    subcall: &Subcall,
    policy: &DomainPolicy,
    upstream_model: &str,
) -> Value {
    let mut tool = json!({"type": "web_search"});
    if let Some(allowed) = policy.request_filter() {
        tool["filters"] = json!({"allowed_domains": allowed});
    }
    json!({
        "model": upstream_model,
        "input": subcall.query,
        "instructions": "Search the web for the query. Reply with the source URLs only.",
        "tools": [tool],
        "tool_choice": "required",
        "stream": true,
        "stream_tool_calls": true,
        "store": false,
        "temperature": 0.1,
        "top_p": 0.95,
        "max_output_tokens": 8192,
    })
}

/// Builds the Meta Responses body that runs one hosted `web_search`, as
/// measured against Meta through `CLIProxyAPI` 8.0.30 (2026-10-15).
///
/// - `tool_choice` is `auto` because Meta rejects every other value with a
///   400. The model may therefore answer without searching; a stream with no
///   search item is a failed search, as on xAI.
/// - `max_tool_calls: 1` ends the response after the one search the harvest
///   needs instead of following it with `open_page` actions: 4.7s and ~2K
///   input tokens end to end, against 23s and ~22K uncapped. The search item
///   arrives at ~2s either way.
/// - `include` asks for `action.sources`, which carry URLs only; titles exist
///   only on the answer's `url_citation` annotations.
/// - No domain filter: Meta's hosted search has none (its Messages surface
///   rejects `allowed_domains` with a 400), so [`DomainPolicy::admits`]
///   alone enforces the policy, as for an xAI allow-list too long to send.
/// - Low reasoning effort, with a pinned `(effort)` suffix dropped: the
///   model only has to issue the query.
pub(super) fn meta_search_request_body(subcall: &Subcall, upstream_model: &str) -> Value {
    let (model, _) = crate::config::split_effort_suffix(upstream_model);
    json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": format!(
                "Search the web for: {}. Return the top results as a list of title + URL.",
                subcall.query
            ),
        }],
        "tools": [{"type": "web_search"}],
        "tool_choice": "auto",
        "max_tool_calls": 1,
        "include": ["web_search_call.action.sources"],
        "reasoning": {"effort": "low"},
        "stream": true,
        "store": false,
    })
}

/// What a chunk fed to [`SourceHarvester`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Harvested {
    /// The first completed hosted search carrying admissible URLs.
    Sources(Vec<Link>),
    /// A terminal event arrived with no admissible URL; the reason is for logs.
    Ended(&'static str),
}

/// Streams a [`NativeSearch`](super::NativeSearch) Responses SSE body and
/// yields the source URLs of the first completed hosted `web_search_call`
/// search that carries any admissible one. xAI and Meta stream the same item
/// shape.
///
/// Event-name agnostic — only the `data:` payload's `type` field is trusted.
/// The child has always been observed to send `event:` lines; tolerating their
/// absence is defensive, since SSE permits it.
pub struct SourceHarvester {
    policy: DomainPolicy,
    events: crate::sse::SseEventBuffer,
}

impl SourceHarvester {
    #[must_use]
    pub fn new(policy: DomainPolicy) -> Self {
        Self {
            policy,
            events: crate::sse::SseEventBuffer::default(),
        }
    }

    /// Feeds raw response bytes in arrival order.
    ///
    /// Returns `Some` exactly once: either the harvest, or the terminal state
    /// that rules one out. Items that complete with no admissible source
    /// (`open_page` actions, fully filtered searches) are skipped and the
    /// stream keeps being read.
    pub fn push(&mut self, chunk: &[u8]) -> Option<Harvested> {
        if self.events.is_disabled() {
            return None;
        }
        if !self.events.push(chunk) {
            return Some(Harvested::Ended("oversized stream"));
        }
        let Self { policy, events } = self;
        let found = events.drain(|event| {
            let payload = crate::sse::parse_event(event)?.data;
            if payload.trim() == "[DONE]" {
                return Some(Harvested::Ended("stream done without sources"));
            }
            let data = serde_json::from_str::<Value>(&payload).ok()?;
            match data.get("type").and_then(Value::as_str) {
                Some("response.output_item.done") => {
                    let links = links_from_item(policy, data.get("item"));
                    (!links.is_empty()).then_some(Harvested::Sources(links))
                }
                Some("response.failed" | "response.error" | "error") => {
                    Some(Harvested::Ended("stream failed"))
                }
                Some("response.completed" | "response.incomplete") => {
                    Some(Harvested::Ended("stream completed without sources"))
                }
                _ => None,
            }
        });
        if found.is_some() {
            // One outcome per stream: everything after it is the drain's
            // business, not the harvest's.
            self.events.disable();
        }
        found
    }
}

/// The admissible, deduplicated URLs of a completed `web_search_call` whose
/// action is a search; the other actions (`open_page`) name a page, not
/// results. Titles do not exist on this path, so the URL is its own label.
fn links_from_item(policy: &DomainPolicy, item: Option<&Value>) -> Vec<Link> {
    let Some(item) = item else {
        return Vec::new();
    };
    if item.get("type").and_then(Value::as_str) != Some("web_search_call")
        || item.get("status").and_then(Value::as_str) != Some("completed")
    {
        return Vec::new();
    }
    let Some(action) = item
        .get("action")
        .filter(|action| action.get("type").and_then(Value::as_str) == Some("search"))
    else {
        return Vec::new();
    };
    let Some(sources) = action.get("sources").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut links = Links::default();
    for source in sources {
        let kind = source.get("type").and_then(Value::as_str);
        if !matches!(kind, None | Some("url")) {
            continue;
        }
        let Some(url) = source.get("url").and_then(Value::as_str) else {
            continue;
        };
        if policy.admits(url) {
            links.push(url, url);
        }
        if links.full() {
            break;
        }
    }
    links.links
}
