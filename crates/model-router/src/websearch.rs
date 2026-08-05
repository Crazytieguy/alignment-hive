//! `WebSearch` sub-call interception for routed GPT models.
//!
//! Claude Code implements its `WebSearch` tool as a side `/v1/messages` call
//! that forces Anthropic's server-side `web_search` tool on the session's
//! main-loop model, then reads links out of `web_search_tool_result` blocks.
//! Routed through `CLIProxyAPI`, the Codex upstream runs the search but returns
//! the links only as inline prose citations, leaving the result block's
//! `content` empty — Claude Code renders that as "No links found." and the
//! call spends an LLM round-trip (20–70s observed) on a search.
//!
//! This module recognizes that sub-call shape and answers it from the
//! requesting agent's own family's search backend:
//!
//! - GPT and open-weights origins: Codex's `/v1/alpha/search` (the endpoint
//!   the Codex CLI's `web.run` tool uses), structured results in ~1–3s, with
//!   the legacy LLM path plus link scraping as the fallback.
//! - Grok origins: xAI's hosted `web_search` tool on `/v1/responses`,
//!   harvesting the source URLs out of the streamed `web_search_call` item.
//!   That path is strict — a search that cannot be performed is reported as a
//!   failed search rather than quietly answered by another vendor.

use bytes::Bytes;
use serde_json::{Value, json};

/// Claude Code's fixed user-message prefix for the `WebSearch` sub-call.
const QUERY_PREFIX: &str = "Perform a web search for the query: ";

/// Upper bound for bodies worth parsing during detection; the sub-call body
/// is a single short message plus one tool schema, while main-loop bodies
/// run to megabytes.
const MAX_DETECT_BODY_BYTES: usize = 128 * 1024;

/// Safety backstop only: every deduplicated link is kept (the search backend
/// returns ~35 ranked results, and the snippet text block describes the same
/// set — capping below it would make the two inconsistent). This bound exists
/// solely so a pathological response cannot balloon the tool result.
const MAX_LINKS: usize = 100;

/// Cap on the text commentary block so a pathological search response cannot
/// balloon the tool result.
const MAX_OUTPUT_TEXT_CHARS: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subcall {
    pub query: String,
    pub user_text: String,
    pub allowed_domains: Option<Vec<String>>,
    pub blocked_domains: Option<Vec<String>>,
    pub stream: bool,
}

/// Recognizes Claude Code's `WebSearch` sub-call: a Messages request whose
/// tools are solely the server-side `web_search_*` tool and whose user
/// message carries the fixed query prefix (observed live on 2.1.217 with
/// `tool_choice: auto`), or that force-invokes the tool outright. Returns
/// `None` for every other request shape (main-loop turns carry the full
/// client tool list).
#[must_use]
pub fn detect(body: &[u8]) -> Option<Subcall> {
    if body.len() > MAX_DETECT_BODY_BYTES || !contains(body, b"\"web_search") {
        return None;
    }
    let document = serde_json::from_slice::<Value>(body).ok()?;
    let is_server_web_search = |tool: &Value| {
        tool.get("name").and_then(Value::as_str) == Some("web_search")
            && tool
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("web_search_"))
    };
    let tools = document.get("tools")?.as_array()?;
    let tool = tools.iter().find(|tool| is_server_web_search(tool))?;
    let user_text = document
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(message_text)?;
    let forced = document.get("tool_choice").is_some_and(|tool_choice| {
        tool_choice.get("type").and_then(Value::as_str) == Some("tool")
            && tool_choice.get("name").and_then(Value::as_str) == Some("web_search")
    });
    let only_web_search_tools = tools.iter().all(is_server_web_search);
    if !(forced || (only_web_search_tools && user_text.starts_with(QUERY_PREFIX))) {
        return None;
    }
    let query = user_text
        .strip_prefix(QUERY_PREFIX)
        .unwrap_or(&user_text)
        .trim()
        .to_string();
    if query.is_empty() {
        return None;
    }
    Some(Subcall {
        query,
        user_text: user_text.clone(),
        allowed_domains: string_list(tool.get("allowed_domains")),
        blocked_domains: string_list(tool.get("blocked_domains")),
        stream: document.get("stream").and_then(Value::as_bool) == Some(true),
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn message_text(message: &Value) -> Option<String> {
    match message.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    let list = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!list.is_empty()).then_some(list)
}

/// Whether this family's searches run on xAI's hosted `web_search` rather
/// than Codex's `/v1/alpha/search`.
///
/// Matched exhaustively on purpose: a new family must decide which backend it
/// belongs to rather than silently inheriting the Codex one.
#[must_use]
pub fn uses_native_search(family: crate::config::ModelFamily) -> bool {
    match family {
        crate::config::ModelFamily::Grok => true,
        crate::config::ModelFamily::Gpt | crate::config::ModelFamily::OpenAiCompat => false,
    }
}

/// The route whose family sends this sub-call to xAI's own search, if any.
///
/// Keyed off the ORIGIN, never the carrier: the carrying request's model is
/// the harness's small-fast pick, not the requesting agent's choice. `Some`
/// only when the origin is unambiguously Grok —
/// - a Claude origin never reaches xAI, including when it reaches this point
///   after a transient Anthropic failure;
/// - an origin whose route has left the config inherits nothing from the
///   carrier;
/// - with no correlation at all, the carrier is the only family signal there
///   is, so it decides.
///
/// `None` leaves every existing path exactly as it was.
pub fn native_search_route<'a>(
    origin: Option<&Origin>,
    carrier: &'a crate::config::ModelRoute,
    lookup: impl FnOnce(&str) -> Option<&'a crate::config::ModelRoute>,
) -> Option<&'a crate::config::ModelRoute> {
    let is_native = |route: &&'a crate::config::ModelRoute| uses_native_search(route.family);
    match origin {
        Some(Origin::Gpt { routing_id }) => lookup(routing_id).filter(is_native),
        Some(Origin::Claude { .. }) => None,
        None => Some(carrier).filter(is_native),
    }
}

/// The Codex slug the alpha-search backend is addressed with when the
/// requesting route is not itself Codex-native.
const ALPHA_SEARCH_DEFAULT_MODEL: &str = "gpt-5.6-sol";

/// The model id to put in an `alpha/search` payload for a request coming
/// from `upstream_model`.
///
/// `/v1/alpha/search` is served by `ChatGPT`'s Codex backend under the Codex
/// credential, and `CLIProxyAPI` forwards the body unchanged — so an
/// open-weights slug has no meaning to it. (Grok origins no longer reach this
/// endpoint at all; see [`uses_native_search`].) The requesting route's own model
/// is only usable when it is Codex-native; otherwise the call is addressed
/// with a known-good Codex slug. This is about the *search backend*, never
/// about which model answers the user.
#[must_use]
pub fn alpha_search_model(upstream_model: &str) -> &str {
    if crate::config::is_codex_native_model(upstream_model) {
        upstream_model
    } else {
        ALPHA_SEARCH_DEFAULT_MODEL
    }
}

/// Builds the `alpha/search` request body (the shape the Codex CLI's
/// `web.run` tool sends; see `codex-rs/codex-api/src/search.rs`).
#[must_use]
pub fn alpha_request_body(subcall: &Subcall, upstream_model: &str) -> Value {
    let mut settings = json!({
        "allowed_callers": ["direct"],
        "external_web_access": true,
    });
    let mut filters = serde_json::Map::new();
    if let Some(allowed) = &subcall.allowed_domains {
        filters.insert("allowed_domains".into(), json!(allowed));
    }
    if let Some(blocked) = &subcall.blocked_domains {
        filters.insert("blocked_domains".into(), json!(blocked));
    }
    if !filters.is_empty() {
        settings["filters"] = Value::Object(filters);
    }
    let mut search_query = json!({"q": subcall.query});
    if let Some(allowed) = &subcall.allowed_domains {
        search_query["domains"] = json!(allowed);
    }
    json!({
        "id": "model-router-websearch",
        "model": upstream_model,
        "input": subcall.user_text,
        "commands": {"search_query": [search_query]},
        "settings": settings,
        "max_output_tokens": 8000,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub title: String,
    pub url: String,
}

impl Link {
    fn block(&self) -> Value {
        json!({"type": "web_search_result", "title": self.title, "url": self.url})
    }
}

/// Extracts deduplicated links from an `alpha/search` `results` array.
#[must_use]
pub fn links_from_alpha_results(results: &[Value]) -> Vec<Link> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for result in results {
        let Some(url) = result.get("url").and_then(Value::as_str) else {
            continue;
        };
        if !seen.insert(url.to_string()) {
            continue;
        }
        let title = result
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .or_else(|| result.get("domain").and_then(Value::as_str))
            .unwrap_or(url);
        links.push(Link {
            title: title.to_string(),
            url: url.to_string(),
        });
        if links.len() >= MAX_LINKS {
            break;
        }
    }
    links
}

// ---------------------------------------------------------------------------
// xAI-native search (Grok origins)
// ---------------------------------------------------------------------------

/// xAI caps a hosted `web_search` allow-list; beyond this the filter is left
/// off the request and enforced locally instead (a rejected field would turn a
/// working search into a failed one).
const MAX_REQUEST_ALLOWED_DOMAINS: usize = 5;

/// The domain constraints a harvested URL must satisfy, taken from the
/// sub-call's `allowed_domains` / `blocked_domains`.
///
/// This is the authoritative filter: the request-side `filters.allowed_domains`
/// only steers the upstream search, and blocked domains are never sent at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomainPolicy {
    allowed: Vec<String>,
    blocked: Vec<String>,
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
#[must_use]
pub fn xai_search_request_body(
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

/// What a chunk fed to [`XaiSourceHarvester`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Harvested {
    /// The first completed hosted search carrying admissible URLs.
    Sources(Vec<Link>),
    /// A terminal event arrived with no admissible URL; the reason is for logs.
    Ended(&'static str),
}

/// Streams an xAI Responses SSE body and yields the source URLs of the first
/// completed hosted `web_search_call` that carries any admissible one.
///
/// Event-name agnostic — only the `data:` payload's `type` field is trusted.
/// The child has always been observed to send `event:` lines; tolerating their
/// absence is defensive, since SSE permits it.
pub struct XaiSourceHarvester {
    policy: DomainPolicy,
    events: crate::usage::SseEventBuffer,
}

impl XaiSourceHarvester {
    #[must_use]
    pub fn new(policy: DomainPolicy) -> Self {
        Self {
            policy,
            events: crate::usage::SseEventBuffer::default(),
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
            let payload = crate::usage::parse_event(event)?.data;
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

/// The admissible, deduplicated URLs of a completed `web_search_call` item.
/// Titles do not exist on this path, so the URL is its own label.
fn links_from_item(policy: &DomainPolicy, item: Option<&Value>) -> Vec<Link> {
    let Some(item) = item else {
        return Vec::new();
    };
    if item.get("type").and_then(Value::as_str) != Some("web_search_call")
        || item.get("status").and_then(Value::as_str) != Some("completed")
    {
        return Vec::new();
    }
    let Some(sources) = item
        .get("action")
        .and_then(|action| action.get("sources"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for source in sources {
        let kind = source.get("type").and_then(Value::as_str);
        if !matches!(kind, None | Some("url")) {
            continue;
        }
        let Some(url) = source.get("url").and_then(Value::as_str) else {
            continue;
        };
        if !policy.admits(url) || !seen.insert(url.to_string()) {
            continue;
        }
        links.push(Link {
            title: url.to_string(),
            url: url.to_string(),
        });
        if links.len() >= MAX_LINKS {
            break;
        }
    }
    links
}

/// Anthropic's error codes for a server-side search that could not run.
pub mod search_error {
    pub const UNAVAILABLE: &str = "unavailable";
    pub const TOO_MANY_REQUESTS: &str = "too_many_requests";
    pub const INVALID_INPUT: &str = "invalid_input";
}

/// Builds the sub-call answer for a search that could not be performed.
///
/// Claude Code renders a `web_search_tool_result` whose `content` is not an
/// array as `Web search error: <error_code>` (verified in the 2.1.222 bundle),
/// so this is a visibly failed search rather than an empty one. The text block
/// carries the detail to the model, and `server_tool_use` is left out of usage
/// entirely: the session's `WebSearch` budget counts successful searches, and
/// xAI's own client does not count failures either.
#[must_use]
pub fn synthesize_error_message(
    model: &str,
    subcall: &Subcall,
    error_code: &str,
    detail: &str,
    input_tokens: u64,
) -> Value {
    let text = format!("Web search failed: {detail}");
    websearch_message(
        model,
        subcall,
        &json!({"type": "web_search_tool_result_error", "error_code": error_code}),
        &text,
        estimated_output_tokens(&text),
        input_tokens,
        // A failed search must not spend the session's WebSearch budget.
        false,
    )
}

/// The router's output-token estimate for a synthesized message.
///
/// Taken from the text the upstream actually produced, not from what survives
/// rendering: the success path's commentary is cleaned of citation markers and
/// `[wordlim: N]` annotations (and may be truncated), none of which changes
/// what the upstream spent.
fn estimated_output_tokens(accounted_text: &str) -> usize {
    (accounted_text.len() / 4).max(1)
}

/// The message envelope both sub-call answers share: the `server_tool_use` +
/// `web_search_tool_result` pair Claude Code reads, optional commentary, and
/// the usage block. `counts_as_search` decides whether the answer reports a
/// `web_search_requests` count at all; `output_tokens` is the caller's
/// estimate, since the text billed for is not always the text shown.
fn websearch_message(
    model: &str,
    subcall: &Subcall,
    result_content: &Value,
    commentary: &str,
    output_tokens: usize,
    input_tokens: u64,
    counts_as_search: bool,
) -> Value {
    let tool_use_id = "srvtoolu_model_router_websearch";
    let mut content = vec![
        json!({
            "type": "server_tool_use",
            "id": tool_use_id,
            "name": "web_search",
            "input": {"query": subcall.query},
        }),
        json!({
            "type": "web_search_tool_result",
            "tool_use_id": tool_use_id,
            "content": result_content.clone(),
        }),
    ];
    if !commentary.trim().is_empty() {
        content.push(json!({"type": "text", "text": commentary}));
    }
    let mut usage = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
    });
    if counts_as_search {
        usage["server_tool_use"] = json!({"web_search_requests": 1});
    }
    json!({
        "id": "msg_model_router_websearch",
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": usage,
    })
}

/// Byte prefilter for the response tap: does this request declare Claude
/// Code's client-side `WebSearch` tool? Keyed on the serialized `"name"`
/// field (the harness emits compact JSON); false positives merely enable the
/// passive tap, false negatives degrade to main-model-matched routing.
#[must_use]
pub fn declares_websearch_tool(body: &[u8]) -> bool {
    memchr::memmem::find(body, b"\"name\":\"WebSearch\"").is_some()
}

/// Extracts the session id from a Messages request's top-level
/// `metadata.user_id` (a JSON-encoded string). The `metadata` value is
/// located with the DOM-free top-level scanner, so multi-megabyte main-loop
/// bodies are never fully parsed and session-id-shaped text inside message
/// content cannot spoof the key.
#[must_use]
pub fn session_id(body: &[u8]) -> Option<String> {
    let range = crate::routing::find_top_level_value_range(body, "metadata")?;
    let metadata = serde_json::from_slice::<Value>(&body[range]).ok()?;
    let user_id = metadata.get("user_id")?.as_str()?;
    let user_id = serde_json::from_str::<Value>(user_id).ok()?;
    user_id.get("session_id")?.as_str().map(ToOwned::to_owned)
}

/// A `WebSearch` tool invocation observed in a model response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SniffedSearch {
    pub query: String,
    pub allowed_domains: Option<Vec<String>>,
    pub blocked_domains: Option<Vec<String>>,
}

impl SniffedSearch {
    fn from_input(input: &Value) -> Option<Self> {
        let query = input.get("query")?.as_str()?.trim().to_string();
        (!query.is_empty()).then(|| Self {
            query,
            allowed_domains: string_list(input.get("allowed_domains")),
            blocked_domains: string_list(input.get("blocked_domains")),
        })
    }
}

/// Cap on one accumulated `tool_use` input.
const MAX_SNIFF_INPUT_BYTES: usize = 64 * 1024;

/// Passive observer of a `/v1/messages` response, yielding `WebSearch` tool
/// invocations as they complete. Feed the raw response bytes in arrival
/// order; the caller must commit the yielded searches BEFORE forwarding the
/// chunk that produced them (the client can act on the completing event the
/// moment it sees it).
pub struct ToolUseSniffer {
    sse: bool,
    events: crate::usage::SseEventBuffer,
    /// SSE: `content_block` index → accumulated `input_json_delta` fragments
    /// for blocks named `WebSearch`.
    pending_inputs: std::collections::HashMap<u64, String>,
}

impl ToolUseSniffer {
    #[must_use]
    pub fn new(sse: bool) -> Self {
        Self {
            sse,
            events: crate::usage::SseEventBuffer::default(),
            pending_inputs: std::collections::HashMap::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<SniffedSearch> {
        if self.events.is_disabled() || !self.events.push(chunk) {
            self.pending_inputs.clear();
            return Vec::new();
        }
        if self.sse {
            self.drain_sse_events()
        } else {
            self.try_parse_json()
        }
    }

    fn drain_sse_events(&mut self) -> Vec<SniffedSearch> {
        let Self {
            events,
            pending_inputs,
            ..
        } = self;
        let mut found = Vec::new();
        // The closure never returns `Some`, so every buffered event is seen.
        events.drain(|event| {
            let data =
                serde_json::from_str::<Value>(&crate::usage::parse_event(event)?.data).ok()?;
            let index = data.get("index").and_then(Value::as_u64);
            match data.get("type").and_then(Value::as_str) {
                Some("content_block_start") => {
                    let block = data.get("content_block")?;
                    if block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && block.get("name").and_then(Value::as_str) == Some("WebSearch")
                        && let Some(index) = index
                    {
                        pending_inputs.insert(index, String::new());
                    }
                }
                Some("content_block_delta") => {
                    if let Some(index) = index
                        && let Some(accumulated) = pending_inputs.get_mut(&index)
                        && let Some(fragment) = data
                            .get("delta")
                            .filter(|delta| {
                                delta.get("type").and_then(Value::as_str)
                                    == Some("input_json_delta")
                            })
                            .and_then(|delta| delta.get("partial_json"))
                            .and_then(Value::as_str)
                    {
                        if accumulated.len().saturating_add(fragment.len()) > MAX_SNIFF_INPUT_BYTES
                        {
                            pending_inputs.remove(&index);
                        } else {
                            accumulated.push_str(fragment);
                        }
                    }
                }
                Some("content_block_stop") => {
                    if let Some(index) = index
                        && let Some(accumulated) = pending_inputs.remove(&index)
                        && let Ok(input) = serde_json::from_str::<Value>(&accumulated)
                        && let Some(search) = SniffedSearch::from_input(&input)
                    {
                        found.push(search);
                    }
                }
                _ => {}
            }
            None::<()>
        });
        found
    }

    fn try_parse_json(&mut self) -> Vec<SniffedSearch> {
        // Trailing whitespace after the closing brace is valid JSON framing.
        let last_meaningful = self
            .events
            .bytes()
            .iter()
            .rev()
            .find(|byte| !byte.is_ascii_whitespace());
        if last_meaningful != Some(&b'}') {
            return Vec::new();
        }
        let Ok(document) = serde_json::from_slice::<Value>(self.events.bytes()) else {
            return Vec::new();
        };
        // One complete document per response: nothing after it is read.
        self.events.disable();
        let Some(content) = document.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };
        content
            .iter()
            .filter(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && block.get("name").and_then(Value::as_str) == Some("WebSearch")
            })
            .filter_map(|block| block.get("input").and_then(SniffedSearch::from_input))
            .collect()
    }
}

/// Builds the normalized Anthropic-native request that answers a sub-call on
/// behalf of a Claude-origin agent. Fields are allowlisted from the original
/// body because its tuning fields (`output_config.effort`, `thinking`, ...)
/// target the session's main-loop model and may be invalid for the origin
/// model; `max_tokens` is clamped to a value every current Claude model
/// accepts (search results fit comfortably), and streaming is disabled so
/// the response can be buffered and re-framed.
#[must_use]
pub fn native_request_body(original: &[u8], origin_model: &str) -> Option<Vec<u8>> {
    const MAX_NATIVE_OUTPUT_TOKENS: u64 = 8192;
    let document = serde_json::from_slice::<Value>(original).ok()?;
    let mut normalized = serde_json::Map::new();
    normalized.insert("model".into(), json!(origin_model));
    normalized.insert("stream".into(), json!(false));
    for field in [
        "max_tokens",
        "messages",
        "system",
        "tools",
        "tool_choice",
        "metadata",
    ] {
        if let Some(value) = document.get(field) {
            normalized.insert(field.into(), value.clone());
        }
    }
    let max_tokens = normalized
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_NATIVE_OUTPUT_TOKENS)
        .min(MAX_NATIVE_OUTPUT_TOKENS);
    normalized.insert("max_tokens".into(), json!(max_tokens));
    serde_json::to_vec(&Value::Object(normalized)).ok()
}

/// Which model asked for a pending web search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Gpt { routing_id: String },
    Claude { model: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PendingKey {
    pub session_id: String,
    pub query: String,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
}

impl PendingKey {
    #[must_use]
    pub fn new(
        session_id: String,
        query: &str,
        allowed_domains: Option<&[String]>,
        blocked_domains: Option<&[String]>,
    ) -> Self {
        fn canonical(domains: Option<&[String]>) -> Vec<String> {
            let mut domains = domains
                .unwrap_or_default()
                .iter()
                .map(|domain| domain.to_lowercase())
                .collect::<Vec<_>>();
            domains.sort();
            domains
        }
        Self {
            session_id,
            query: query.trim().to_string(),
            allowed_domains: canonical(allowed_domains),
            blocked_domains: canonical(blocked_domains),
        }
    }
}

/// Entries expire after this long; the harness issues the sub-call within
/// seconds of the `tool_use` reaching it.
const PENDING_TTL: std::time::Duration = std::time::Duration::from_mins(2);
/// Total queued entries across all keys; oldest dropped first.
const MAX_PENDING_TOTAL: usize = 256;

struct PendingEntry {
    origin: Origin,
    inserted: std::time::Instant,
}

/// Observed-but-not-yet-consumed `WebSearch` invocations, keyed by session,
/// query, and domain filters. Same-key entries queue FIFO so concurrent
/// identical queries degrade to an ordering heuristic instead of losing
/// entries.
#[derive(Default)]
pub struct PendingSearches {
    inner: std::sync::Mutex<
        std::collections::HashMap<PendingKey, std::collections::VecDeque<PendingEntry>>,
    >,
}

impl PendingSearches {
    pub fn insert(&self, key: PendingKey, origin: Origin) {
        let now = std::time::Instant::now();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.retain(|_, queue| {
            queue.retain(|entry| now.duration_since(entry.inserted) < PENDING_TTL);
            !queue.is_empty()
        });
        let total: usize = inner.values().map(std::collections::VecDeque::len).sum();
        if total >= MAX_PENDING_TOTAL {
            // Drop the globally oldest entry.
            if let Some(oldest_key) = inner
                .iter()
                .filter_map(|(key, queue)| queue.front().map(|entry| (key, entry.inserted)))
                .min_by_key(|(_, inserted)| *inserted)
                .map(|(key, _)| key.clone())
                && let Some(queue) = inner.get_mut(&oldest_key)
            {
                queue.pop_front();
                if queue.is_empty() {
                    inner.remove(&oldest_key);
                }
            }
        }
        inner.entry(key).or_default().push_back(PendingEntry {
            origin,
            inserted: now,
        });
    }

    pub fn consume(&self, key: &PendingKey) -> Option<Origin> {
        let now = std::time::Instant::now();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue = inner.get_mut(key)?;
        while let Some(entry) = queue.pop_front() {
            if now.duration_since(entry.inserted) < PENDING_TTL {
                if queue.is_empty() {
                    inner.remove(key);
                }
                return Some(entry.origin);
            }
        }
        inner.remove(key);
        None
    }
}

/// Strips the search backend's private-use citation markers
/// (`U+E200 … U+E201`, with `U+E202` separators) and `[wordlim: N]`
/// annotations from rendered search output, and truncates on a char
/// boundary.
#[must_use]
pub fn clean_search_output(output: &str) -> String {
    let mut cleaned = String::with_capacity(output.len());
    let mut in_marker = false;
    for character in output.chars() {
        match character {
            '\u{E200}' => in_marker = true,
            '\u{E201}' => in_marker = false,
            '\u{E000}'..='\u{F8FF}' => {}
            _ if !in_marker => cleaned.push(character),
            _ => {}
        }
    }
    let mut result = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while let Some(start) = rest.find("[wordlim:") {
        let Some(length) = rest[start..].find(']') else {
            break;
        };
        result.push_str(&rest[..start]);
        rest = &rest[start + length + 1..];
    }
    result.push_str(rest);
    if result.chars().count() > MAX_OUTPUT_TEXT_CHARS {
        result = result.chars().take(MAX_OUTPUT_TEXT_CHARS).collect();
        result.push_str("\n[truncated]");
    }
    result
}

/// Builds a complete Anthropic message answering the sub-call: the
/// `server_tool_use` + `web_search_tool_result` pair Claude Code parses
/// links from, plus the cleaned search output as commentary.
#[must_use]
pub fn synthesize_message(
    model: &str,
    subcall: &Subcall,
    links: &[Link],
    output_text: &str,
    input_tokens: u64,
) -> Value {
    websearch_message(
        model,
        subcall,
        &json!(links.iter().map(Link::block).collect::<Vec<_>>()),
        &clean_search_output(output_text),
        // Accounted from what the upstream produced, not from what survives
        // cleaning and truncation.
        estimated_output_tokens(output_text),
        input_tokens,
        true,
    )
}

/// Scrapes `[text](url)` markdown links and bare URLs from prose, in order,
/// deduplicated, with `utm_source=openai` tracking stripped.
#[must_use]
pub fn scrape_links(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |title: &str, url: &str| {
        let url = strip_openai_tracking(url);
        if seen.insert(url.clone()) && links.len() < MAX_LINKS {
            let title = if title.trim().is_empty() {
                domain_of(&url).unwrap_or(url.as_str()).to_string()
            } else {
                title.trim().to_string()
            };
            links.push(Link { title, url });
        }
    };
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        let Some(title_end) = rest.find(']') else {
            break;
        };
        let title = &rest[..title_end];
        let after = &rest[title_end + 1..];
        if let Some(url_body) = after.strip_prefix('(')
            && let Some(url_end) = url_body.find(')')
        {
            let url = url_body[..url_end].trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                push(title, url);
                rest = &url_body[url_end + 1..];
                continue;
            }
        }
        rest = after;
    }
    // Bare URLs outside markdown syntax.
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        let candidate = &rest[start..];
        if candidate.starts_with("http://") || candidate.starts_with("https://") {
            let end = candidate
                .find(|character: char| {
                    character.is_whitespace() || "()<>[]\"'".contains(character)
                })
                .unwrap_or(candidate.len());
            let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            if !url.is_empty() && !rest[..start].ends_with('(') {
                push("", url);
            }
            rest = &candidate[end.max(1)..];
        } else {
            rest = &candidate[4..];
        }
    }
    links
}

fn strip_openai_tracking(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept = query
        .split('&')
        .filter(|parameter| *parameter != "utm_source=openai")
        .collect::<Vec<_>>()
        .join("&");
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{kept}")
    }
}

fn domain_of(url: &str) -> Option<&str> {
    url.split_once("://")?.1.split(['/', '?', '#']).next()
}

/// Fills the first empty `web_search_tool_result` block of a legacy-path
/// response with links scraped from the message's text blocks. Returns the
/// number of links written (0 leaves the message untouched).
pub fn fill_empty_web_search_results(message: &mut Value) -> usize {
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return 0;
    };
    let text = content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let links = scrape_links(&text);
    if links.is_empty() {
        return 0;
    }
    let empty_result = content.iter_mut().find(|block| {
        block.get("type").and_then(Value::as_str) == Some("web_search_tool_result")
            && block
                .get("content")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
    });
    let Some(block) = empty_result else {
        return 0;
    };
    block["content"] = Value::Array(links.iter().map(Link::block).collect());
    links.len()
}

/// Renders a complete Anthropic message as the SSE event sequence a
/// streaming client expects. Tool and result blocks arrive whole (matching
/// how the real API streams `web_search_tool_result`); text and thinking
/// blocks get a single delta.
#[must_use]
pub fn message_to_sse(message: &Value) -> Vec<Bytes> {
    let mut events = Vec::new();
    let mut push = |event: &str, data: &Value| {
        events.push(Bytes::from(format!("event: {event}\ndata: {data}\n\n")));
    };
    let mut skeleton = message.clone();
    skeleton["content"] = json!([]);
    skeleton["stop_reason"] = Value::Null;
    if let Some(usage) = skeleton.get_mut("usage").and_then(Value::as_object_mut) {
        usage.insert("output_tokens".into(), json!(0));
    }
    push(
        "message_start",
        &json!({"type": "message_start", "message": skeleton}),
    );

    let empty = Vec::new();
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for (index, block) in content.iter().enumerate() {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                push(
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": index,
                        "content_block": {"type": "text", "text": ""}}),
                );
                push(
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": index,
                        "delta": {"type": "text_delta", "text": text}}),
                );
            }
            "server_tool_use" | "tool_use" => {
                let mut start = block.clone();
                start["input"] = json!({});
                push(
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": index, "content_block": start}),
                );
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                push(
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
                );
            }
            _ => {
                push(
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": index, "content_block": block}),
                );
            }
        }
        push(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        );
    }

    push(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": message.get("stop_reason").cloned().unwrap_or(json!("end_turn")),
                "stop_sequence": null,
            },
            "usage": message.get("usage").cloned().unwrap_or_else(|| json!({})),
        }),
    );
    push("message_stop", &json!({"type": "message_stop"}));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subcall_body(stream: bool) -> Vec<u8> {
        json!({
            "model": "gpt-5.6-sol",
            "max_tokens": 4096,
            "stream": stream,
            "messages": [{"role": "user", "content": "Perform a web search for the query: rust axum"}],
            "system": [{"type": "text", "text": "You are an assistant for performing a web search tool use"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8,
                       "allowed_domains": ["docs.rs"]}],
            "tool_choice": {"type": "tool", "name": "web_search"},
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn detects_the_websearch_subcall_shape() {
        let subcall = detect(&subcall_body(true)).unwrap();
        assert_eq!(subcall.query, "rust axum");
        assert_eq!(
            subcall.allowed_domains.as_deref(),
            Some(&["docs.rs".to_string()][..])
        );
        assert_eq!(subcall.blocked_domains, None);
        assert!(subcall.stream);
    }

    #[test]
    fn detect_handles_content_block_arrays_and_missing_prefix() {
        let body = json!({
            "model": "gpt-5.6-sol",
            "messages": [{"role": "user",
                "content": [{"type": "text", "text": "just search this"}]}],
            "tools": [{"type": "web_search_20260209", "name": "web_search"}],
            "tool_choice": {"type": "tool", "name": "web_search"},
        })
        .to_string();
        let subcall = detect(body.as_bytes()).unwrap();
        assert_eq!(subcall.query, "just search this");
        assert!(!subcall.stream);
    }

    #[test]
    fn detects_the_live_auto_tool_choice_shape() {
        // The shape captured from Claude Code 2.1.217: tool_choice auto, the
        // server tool as the only tools entry, prefixed query.
        let body = json!({
            "model": "gpt-5.6-sol",
            "max_tokens": 32000,
            "stream": true,
            "messages": [{"role": "user", "content": [{"type": "text",
                "text": "Perform a web search for the query: rust axum"}]}],
            "system": [{"type": "text", "text": "You are a Claude agent"},
                       {"type": "text",
                        "text": "You are an assistant for performing a web search tool use"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
            "tool_choice": {"type": "auto"},
            "output_config": {"effort": "medium"},
        })
        .to_string();
        let subcall = detect(body.as_bytes()).unwrap();
        assert_eq!(subcall.query, "rust axum");
        assert!(subcall.stream);
    }

    #[test]
    fn ignores_regular_requests_declaring_the_client_websearch_tool() {
        for body in [
            // Main-loop turn: WebSearch as a plain client tool, no forced choice.
            json!({
                "model": "gpt-5.6-sol",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"name": "WebSearch", "input_schema": {"type": "object"}}],
            }),
            // Forced choice of a non-server tool named web_search.
            json!({
                "model": "gpt-5.6-sol",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"name": "web_search", "input_schema": {"type": "object"}}],
                "tool_choice": {"type": "tool", "name": "web_search"},
            }),
            // Server tool declared but not forced.
            json!({
                "model": "gpt-5.6-sol",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "web_search_20250305", "name": "web_search"}],
                "tool_choice": {"type": "auto"},
            }),
        ] {
            assert_eq!(detect(body.to_string().as_bytes()), None);
        }
    }

    #[test]
    fn oversized_bodies_are_skipped_without_parsing() {
        let mut body = subcall_body(true);
        body.extend(std::iter::repeat_n(b' ', MAX_DETECT_BODY_BYTES));
        assert_eq!(detect(&body), None);
    }

    #[test]
    fn alpha_request_carries_query_domains_and_input() {
        let subcall = detect(&subcall_body(false)).unwrap();
        let body = alpha_request_body(&subcall, "gpt-5.6-sol-upstream");
        assert_eq!(body["model"], "gpt-5.6-sol-upstream");
        assert_eq!(body["commands"]["search_query"][0]["q"], "rust axum");
        assert_eq!(body["commands"]["search_query"][0]["domains"][0], "docs.rs");
        assert_eq!(body["settings"]["filters"]["allowed_domains"][0], "docs.rs");
        assert_eq!(
            body["input"],
            "Perform a web search for the query: rust axum"
        );
    }

    #[test]
    fn alpha_results_map_to_deduplicated_links() {
        let results = vec![
            json!({"type": "text_result", "title": "Axum", "url": "https://docs.rs/axum",
                   "domain": "docs.rs"}),
            json!({"type": "text_result", "title": "Axum again", "url": "https://docs.rs/axum"}),
            json!({"type": "text_result", "title": "", "url": "https://tokio.rs/",
                   "domain": "tokio.rs"}),
            json!({"type": "image_result"}),
        ];
        let links = links_from_alpha_results(&results);
        assert_eq!(
            links,
            vec![
                Link {
                    title: "Axum".into(),
                    url: "https://docs.rs/axum".into()
                },
                Link {
                    title: "tokio.rs".into(),
                    url: "https://tokio.rs/".into()
                },
            ]
        );
    }

    #[test]
    fn cleans_citation_markers_and_wordlim_annotations() {
        let raw = "Title (https://a.example)\n\u{E200}cite\u{E202}turn0search0\u{E201} \
                   [wordlim: 200] Published: today; body text.";
        let cleaned = clean_search_output(raw);
        assert_eq!(
            cleaned,
            "Title (https://a.example)\n  Published: today; body text."
        );
    }

    #[test]
    fn scrapes_markdown_and_bare_links_with_tracking_stripped() {
        let text = "See ([docs.rs](https://docs.rs/axum?utm_source=openai)) and \
                    [Tokio shutdown](https://tokio.rs/tokio/topics/shutdown) plus \
                    bare https://bun.sh/blog. Also https://docs.rs/axum again.";
        let links = scrape_links(text);
        assert_eq!(
            links,
            vec![
                Link {
                    title: "docs.rs".into(),
                    url: "https://docs.rs/axum".into()
                },
                Link {
                    title: "Tokio shutdown".into(),
                    url: "https://tokio.rs/tokio/topics/shutdown".into(),
                },
                Link {
                    title: "bun.sh".into(),
                    url: "https://bun.sh/blog".into()
                },
            ]
        );
    }

    #[test]
    fn fills_only_an_empty_result_block() {
        let mut message = json!({
            "content": [
                {"type": "web_search_tool_result", "tool_use_id": "a", "content": []},
                {"type": "web_search_tool_result", "tool_use_id": "b",
                 "content": [{"type": "web_search_result", "title": "kept",
                              "url": "https://kept.example"}]},
                {"type": "text", "text": "Summary ([site](https://site.example/page))"},
            ],
        });
        assert_eq!(fill_empty_web_search_results(&mut message), 1);
        assert_eq!(
            message["content"][0]["content"][0]["url"],
            "https://site.example/page"
        );
        assert_eq!(message["content"][1]["content"][0]["title"], "kept");
    }

    #[test]
    fn linkless_responses_are_left_untouched() {
        let mut message = json!({
            "content": [
                {"type": "web_search_tool_result", "tool_use_id": "a", "content": []},
                {"type": "text", "text": "It is sunny, 28 degrees."},
            ],
        });
        assert_eq!(fill_empty_web_search_results(&mut message), 0);
        assert_eq!(message["content"][0]["content"], json!([]));
    }

    #[test]
    fn synthesized_message_streams_as_a_valid_event_sequence() {
        let subcall = detect(&subcall_body(true)).unwrap();
        let links = vec![Link {
            title: "Axum".into(),
            url: "https://docs.rs/axum".into(),
        }];
        let message = synthesize_message("gpt-5.6-sol", &subcall, &links, "found it", 42);
        assert_eq!(message["content"][0]["type"], "server_tool_use");
        assert_eq!(message["content"][1]["type"], "web_search_tool_result");
        assert_eq!(
            message["content"][1]["content"][0]["url"],
            "https://docs.rs/axum"
        );
        assert_eq!(message["content"][2]["type"], "text");
        assert_eq!(message["usage"]["input_tokens"], 42);

        let body = message_to_sse(&message)
            .iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
            .collect::<String>();
        let positions = [
            "event: message_start",
            "event: content_block_start",
            "event: content_block_delta",
            "event: message_delta",
            "event: message_stop",
        ]
        .map(|event| body.find(event).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(body.contains("input_json_delta"));
        assert!(body.contains("web_search_tool_result"));
    }

    #[test]
    fn session_id_is_read_from_top_level_metadata() {
        let body = json!({
            "model": "gpt-5.6-sol",
            "metadata": {"user_id":
                "{\"device_id\":\"abc\",\"session_id\":\"8c259a6a-fc88-47d0\"}"},
            "messages": [],
        })
        .to_string();
        assert_eq!(
            session_id(body.as_bytes()).as_deref(),
            Some("8c259a6a-fc88-47d0")
        );
        assert_eq!(session_id(b"{\"messages\":[]}"), None);
    }

    #[test]
    fn session_id_ignores_decoys_in_message_content_even_in_large_bodies() {
        // A session inspecting router captures can carry serialized metadata
        // inside message text; only the genuine top-level metadata counts.
        let decoy = "here is a capture: {\\\"user_id\\\": \\\"{\\\\\\\"session_id\\\\\\\":\\\\\\\"decoy-session\\\\\\\"}\\\"}";
        let padding = "x".repeat(300 * 1024);
        let body = format!(
            "{{\"model\":\"gpt-5.6-sol\",\"messages\":[{{\"role\":\"user\",\"content\":\"{decoy} {padding}\"}}],\"metadata\":{{\"user_id\":\"{{\\\"session_id\\\":\\\"real-session\\\"}}\"}}}}"
        );
        // Sanity: the decoy pattern appears before the real metadata.
        assert!(body.contains("decoy-session"));
        assert!(body.len() > 300 * 1024);
        assert_eq!(session_id(body.as_bytes()).as_deref(), Some("real-session"));
    }

    #[test]
    fn declares_websearch_tool_matches_compact_tool_entries() {
        assert!(declares_websearch_tool(
            br#"{"tools":[{"name":"WebSearch","input_schema":{}}]}"#
        ));
        assert!(!declares_websearch_tool(
            br#"{"tools":[{"name":"Read"}],"messages":[{"content":"WebSearch is a tool"}]}"#
        ));
    }

    fn sse_event(data: &Value) -> String {
        format!("event: x\ndata: {data}\n\n")
    }

    #[test]
    fn sniffer_accumulates_split_sse_deltas_and_ignores_other_tools() {
        let events = [
            sse_event(&json!({"type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"t1","name":"WebSearch","input":{}}})),
            sse_event(&json!({"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"ru"}})),
            sse_event(&json!({"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"st axum\"}"}})),
            sse_event(&json!({"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"t2","name":"Bash","input":{}}})),
            sse_event(&json!({"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}})),
            sse_event(&json!({"type":"content_block_stop","index":1})),
            sse_event(&json!({"type":"content_block_stop","index":0})),
        ]
        .concat();
        // Feed in 7-byte chunks to exercise event reassembly across chunk
        // boundaries.
        let mut sniffer = ToolUseSniffer::new(true);
        let mut found = Vec::new();
        for chunk in events.as_bytes().chunks(7) {
            found.extend(sniffer.push(chunk));
        }
        assert_eq!(
            found,
            vec![SniffedSearch {
                query: "rust axum".into(),
                allowed_domains: None,
                blocked_domains: None,
            }]
        );
    }

    #[test]
    fn sniffer_reads_non_streaming_json_bodies_once() {
        let body = json!({
            "type": "message",
            "content": [
                {"type": "text", "text": "searching"},
                {"type": "tool_use", "id": "t1", "name": "WebSearch",
                 "input": {"query": "bun release", "allowed_domains": ["bun.sh"]}},
            ],
        })
        .to_string();
        let mut sniffer = ToolUseSniffer::new(false);
        let (first, second) = body.as_bytes().split_at(body.len() / 2);
        assert_eq!(sniffer.push(first), vec![]);
        let found = sniffer.push(second);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].query, "bun release");
        assert_eq!(
            found[0].allowed_domains.as_deref(),
            Some(&["bun.sh".to_string()][..])
        );
        // A second complete document is not re-parsed.
        assert_eq!(sniffer.push(body.as_bytes()), vec![]);
    }

    #[test]
    fn sniffer_accepts_json_bodies_with_trailing_whitespace() {
        let body = json!({
            "type": "message",
            "content": [{"type": "tool_use", "id": "t1", "name": "WebSearch",
                         "input": {"query": "bun release"}}],
        })
        .to_string()
            + "\r\n";
        let mut sniffer = ToolUseSniffer::new(false);
        let found = sniffer.push(body.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].query, "bun release");
    }

    #[test]
    fn sniffer_disables_itself_on_oversized_buffers() {
        let mut sniffer = ToolUseSniffer::new(true);
        let big = vec![b'x'; crate::usage::MAX_SCAN_BUFFER_BYTES + 1];
        assert_eq!(sniffer.push(&big), vec![]);
        // Later well-formed events are ignored once disabled.
        let event = sse_event(&json!({"type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"t","name":"WebSearch","input":{}}}));
        assert_eq!(sniffer.push(event.as_bytes()), vec![]);
    }

    #[test]
    fn pending_searches_queue_fifo_and_expire() {
        let pending = PendingSearches::default();
        let key = PendingKey::new("session".into(), " q ", None, None);
        assert_eq!(key.query, "q");
        pending.insert(
            key.clone(),
            Origin::Claude {
                model: "haiku".into(),
            },
        );
        pending.insert(
            key.clone(),
            Origin::Gpt {
                routing_id: "gpt-5.6-sol".into(),
            },
        );
        assert_eq!(
            pending.consume(&key),
            Some(Origin::Claude {
                model: "haiku".into()
            })
        );
        assert_eq!(
            pending.consume(&key),
            Some(Origin::Gpt {
                routing_id: "gpt-5.6-sol".into()
            })
        );
        assert_eq!(pending.consume(&key), None);
    }

    #[test]
    fn pending_key_canonicalizes_domains() {
        let a = PendingKey::new(
            "s".into(),
            "q",
            Some(&["B.com".into(), "a.com".into()]),
            None,
        );
        let b = PendingKey::new(
            "s".into(),
            "q",
            Some(&["a.com".into(), "b.com".into()]),
            None,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn pending_searches_enforce_the_total_cap() {
        let pending = PendingSearches::default();
        for i in 0..=MAX_PENDING_TOTAL {
            let key = PendingKey::new("s".into(), &format!("q{i}"), None, None);
            pending.insert(key, Origin::Claude { model: "m".into() });
        }
        // The oldest entry (q0) was evicted to admit the newest.
        let oldest = PendingKey::new("s".into(), "q0", None, None);
        assert_eq!(pending.consume(&oldest), None);
        let newest = PendingKey::new("s".into(), &format!("q{MAX_PENDING_TOTAL}"), None, None);
        assert!(pending.consume(&newest).is_some());
    }

    // ---- xAI-native search ----

    const BASIC_FIXTURE: &str = include_str!("testdata/xai-websearch-basic.sse");
    const DATA_ONLY_FIXTURE: &str = include_str!("testdata/xai-websearch-dataonly.sse");

    fn harvest_all(fixture: &str, policy: DomainPolicy, chunk_size: usize) -> Option<Harvested> {
        let mut harvester = XaiSourceHarvester::new(policy);
        let mut outcome = None;
        for chunk in fixture.as_bytes().chunks(chunk_size) {
            if let Some(found) = harvester.push(chunk) {
                outcome = Some(found);
                break;
            }
        }
        outcome
    }

    fn xai_event(data: &Value) -> String {
        format!("event: x\ndata: {data}\n\n")
    }

    fn search_item(sources: &[Value]) -> Value {
        json!({"type": "response.output_item.done", "item": {
            "id": "ws_1", "type": "web_search_call", "status": "completed",
            "action": {"type": "search", "query": "q", "sources": sources}}})
    }

    #[test]
    fn harvests_sources_from_the_captured_stream() {
        // The fixture is a byte-faithful excerpt of a real child response.
        let Some(Harvested::Sources(links)) =
            harvest_all(BASIC_FIXTURE, DomainPolicy::default(), 64 * 1024)
        else {
            panic!("expected a harvest from the captured stream");
        };
        assert_eq!(links.len(), 10);
        assert_eq!(
            links[0].url,
            "https://github.com/tokio-rs/axum/blob/main/examples/graceful-shutdown/src/main.rs"
        );
        // No titles exist on this path: the URL is its own label.
        assert!(links.iter().all(|link| link.title == link.url));
        // The harvest fires before the stream is exhausted: the fixture's
        // trailing output_text deltas are never needed.
        assert!(BASIC_FIXTURE.contains("response.output_text.delta"));
    }

    #[test]
    fn harvests_across_chunk_boundaries() {
        let Some(Harvested::Sources(links)) =
            harvest_all(BASIC_FIXTURE, DomainPolicy::default(), 7)
        else {
            panic!("expected a harvest when fed in 7-byte chunks");
        };
        assert_eq!(links.len(), 10);
    }

    #[test]
    fn harvests_from_data_only_events_and_joins_split_data_lines() {
        // Defensive coverage: the child has not been observed to omit `event:`
        // lines or split one event's JSON across two `data:` lines, but SSE
        // permits both.
        let Some(Harvested::Sources(links)) =
            harvest_all(DATA_ONLY_FIXTURE, DomainPolicy::default(), 5)
        else {
            panic!("expected a harvest from the data-only fixture");
        };
        assert_eq!(
            links
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            ["https://example.test/a", "https://example.test/b"]
        );
    }

    #[test]
    fn skips_source_less_items_and_takes_the_first_search_with_sources() {
        let stream = [
            xai_event(&json!({"type": "response.output_item.done", "item": {
                "type": "web_search_call", "status": "completed",
                "action": {"type": "open_page", "url": "https://a.example"}}})),
            xai_event(&search_item(&[
                json!({"type": "url", "url": "https://first.example/1"}),
            ])),
            xai_event(&search_item(&[
                json!({"type": "url", "url": "https://second.example/2"}),
            ])),
        ]
        .concat();
        let Some(Harvested::Sources(links)) = harvest_all(&stream, DomainPolicy::default(), 9)
        else {
            panic!("expected the first sources-bearing item");
        };
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://first.example/1");
    }

    #[test]
    fn ignores_incomplete_items_and_foreign_tool_calls() {
        let stream = [
            // Still running.
            xai_event(&json!({"type": "response.output_item.done", "item": {
                "type": "web_search_call", "status": "in_progress",
                "action": {"type": "search", "sources": [{"type": "url", "url": "https://early.example"}]}}})),
            // x_search shape: a custom tool call, never a web_search_call.
            xai_event(&json!({"type": "response.output_item.done", "item": {
                "type": "custom_tool_call", "status": "completed", "name": "x_search",
                "input": "{\"query\":\"q\"}"}})),
            // The progress signal carries no sources.
            xai_event(&json!({"type": "response.web_search_call.completed", "item_id": "ws_1"})),
            xai_event(&json!({"type": "response.completed"})),
        ]
        .concat();
        assert_eq!(
            harvest_all(&stream, DomainPolicy::default(), 13),
            Some(Harvested::Ended("stream completed without sources"))
        );
    }

    #[test]
    fn terminal_events_without_sources_are_reported() {
        for (event, reason) in [
            (
                xai_event(&json!({"type": "response.completed"})),
                "stream completed without sources",
            ),
            (
                xai_event(&json!({"type": "response.failed", "error": {"code": "x"}})),
                "stream failed",
            ),
            (
                "event: x\ndata: [DONE]\n\n".to_string(),
                "stream done without sources",
            ),
        ] {
            assert_eq!(
                harvest_all(&event, DomainPolicy::default(), 11),
                Some(Harvested::Ended(reason)),
                "{event}"
            );
        }
    }

    #[test]
    fn deduplicates_and_drops_non_url_entries() {
        let stream = xai_event(&search_item(&[
            json!({"type": "url", "url": "https://a.example/x"}),
            json!({"type": "url", "url": "https://a.example/x"}),
            json!({"type": "x_post", "url": "https://x.com/post/1"}),
            json!({"type": "url", "url": "ftp://a.example/file"}),
            json!({"type": "url"}),
            json!({"url": "https://b.example/y"}),
        ]));
        let Some(Harvested::Sources(links)) = harvest_all(&stream, DomainPolicy::default(), 17)
        else {
            panic!("expected a harvest");
        };
        assert_eq!(
            links
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            ["https://a.example/x", "https://b.example/y"]
        );
    }

    #[test]
    fn oversized_streams_end_the_harvest() {
        let mut harvester = XaiSourceHarvester::new(DomainPolicy::default());
        let big = vec![b'x'; crate::usage::MAX_SCAN_BUFFER_BYTES + 1];
        assert_eq!(
            harvester.push(&big),
            Some(Harvested::Ended("oversized stream"))
        );
        // Later well-formed events are ignored once disabled.
        let event = xai_event(&search_item(&[
            json!({"type": "url", "url": "https://a.example"}),
        ]));
        assert_eq!(harvester.push(event.as_bytes()), None);
    }

    #[test]
    fn xai_body_carries_the_query_tool_and_streaming_fields() {
        let subcall = detect(&subcall_body(true)).unwrap();
        let policy = DomainPolicy::from_subcall(&subcall).unwrap();
        let body = xai_search_request_body(&subcall, &policy, "grok-4.5");
        assert_eq!(body["model"], "grok-4.5");
        assert_eq!(body["input"], "rust axum");
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_tool_calls"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["temperature"], 0.1);
        assert_eq!(body["top_p"], 0.95);
        assert_eq!(body["max_output_tokens"], 8192);
        // A short allow-list is sent as a filter; the sub-call fixture has one.
        assert_eq!(body["tools"][0]["filters"]["allowed_domains"][0], "docs.rs");
    }

    #[test]
    fn oversized_or_absent_allow_lists_send_no_filter() {
        let mut subcall = detect(&subcall_body(true)).unwrap();
        subcall.allowed_domains = Some(
            (0..=MAX_REQUEST_ALLOWED_DOMAINS)
                .map(|index| format!("d{index}.example"))
                .collect(),
        );
        let policy = DomainPolicy::from_subcall(&subcall).unwrap();
        assert!(
            xai_search_request_body(&subcall, &policy, "grok-4.5")["tools"][0]
                .get("filters")
                .is_none()
        );
        subcall.allowed_domains = None;
        subcall.blocked_domains = Some(vec!["blocked.example".into()]);
        // Blocked domains are never sent upstream — only enforced locally.
        let policy = DomainPolicy::from_subcall(&subcall).unwrap();
        let body = xai_search_request_body(&subcall, &policy, "grok-4.5");
        assert!(body["tools"][0].get("filters").is_none());
        assert!(!body.to_string().contains("blocked.example"));
    }

    #[test]
    fn domain_rules_and_hosts_canonicalize_the_same_way() {
        let policy = |allowed: &[&str], blocked: &[&str]| {
            DomainPolicy::from_subcall(&Subcall {
                query: "q".into(),
                user_text: "q".into(),
                allowed_domains: (!allowed.is_empty())
                    .then(|| allowed.iter().map(|d| (*d).to_string()).collect()),
                blocked_domains: (!blocked.is_empty())
                    .then(|| blocked.iter().map(|d| (*d).to_string()).collect()),
                stream: false,
            })
            .unwrap()
        };
        // A trailing DNS root dot is the same host, on either side of the
        // comparison, and must not slip past a block-list.
        let blocked = policy(&[], &["example.com"]);
        assert!(!blocked.admits("https://example.com./page"));
        assert!(!blocked.admits("https://www.example.com./page"));
        assert!(blocked.admits("https://example.org/page"));
        let blocked_with_dot = policy(&[], &["example.com."]);
        assert!(!blocked_with_dot.admits("https://example.com/page"));
        assert!(!blocked_with_dot.admits("https://example.com./page"));
        // A rule spelled in Unicode matches the punycode host the URL parser
        // produces, and vice versa.
        let unicode_rule = policy(&["bücher.example"], &[]);
        assert!(unicode_rule.admits("https://xn--bcher-kva.example/page"));
        assert!(unicode_rule.admits("https://bücher.example/page"));
        let punycode_rule = policy(&["xn--bcher-kva.example"], &[]);
        assert!(punycode_rule.admits("https://bücher.example/page"));
        assert!(!punycode_rule.admits("https://other.example/page"));
    }

    #[test]
    fn an_unusable_domain_filter_is_an_error_not_an_open_filter() {
        let subcall = |allowed: Option<Vec<String>>, blocked: Option<Vec<String>>| Subcall {
            query: "q".into(),
            user_text: "q".into(),
            allowed_domains: allowed,
            blocked_domains: blocked,
            stream: false,
        };
        // A malformed allow-list must never collapse into allow-all.
        for rule in [
            "https://example.com/path",
            "example.com/path",
            "user@example.com",
            "example.com:8443",
            "  ",
            "..",
            // For special schemes the URL parser treats a backslash as a path
            // separator, so these would otherwise canonicalize to
            // `example.com` — turning a malformed restriction into permission
            // for the whole domain.
            "example.com\\path",
            "example.com\\",
            "example.com\\\\evil.example",
            // The parser strips tabs and newlines outright, so these would
            // otherwise canonicalize to `example.com` as well.
            "exa\tmple.com",
            "exa\nmple.com",
            "exa\rmple.com",
            "example.com\u{0}",
            "exam\u{7}ple.com",
        ] {
            let call = subcall(Some(vec![rule.to_string()]), None);
            assert_eq!(
                DomainPolicy::from_subcall(&call),
                Err(InvalidDomainRule(rule.to_string())),
                "rule {rule:?} must be rejected"
            );
        }
        // Nor may a malformed block-list quietly admit what it excluded.
        assert!(DomainPolicy::from_subcall(&subcall(None, Some(vec!["a b".into()]))).is_err());
        // A leading dot is a normal spelling, not an error.
        let policy =
            DomainPolicy::from_subcall(&subcall(Some(vec![".example.com".into()]), None)).unwrap();
        assert!(policy.admits("https://www.example.com/page"));
    }

    #[test]
    fn domain_policy_admits_by_host_not_by_substring() {
        let policy = |allowed: &[&str], blocked: &[&str]| DomainPolicy {
            allowed: allowed.iter().map(|d| (*d).to_string()).collect(),
            blocked: blocked.iter().map(|d| (*d).to_string()).collect(),
        };
        let allow = policy(&["allowed.example"], &[]);
        assert!(allow.admits("https://allowed.example/page"));
        assert!(allow.admits("https://docs.allowed.example/page"));
        assert!(allow.admits("https://ALLOWED.example/page"));
        assert!(allow.admits("https://allowed.example:8443/page"));
        // Deceptive suffix and userinfo host spoofing must not pass.
        assert!(!allow.admits("https://allowed.example.evil.example/page"));
        assert!(!allow.admits("https://allowed.example@evil.example/page"));
        assert!(!allow.admits("https://notallowed.example/page"));
        assert!(!allow.admits("ftp://allowed.example/file"));
        assert!(!allow.admits("not a url"));
        assert!(!allow.admits("/relative/path"));
        // Blocked wins over allowed.
        let both = policy(&["a.example"], &["bad.a.example"]);
        assert!(both.admits("https://a.example/x"));
        assert!(!both.admits("https://bad.a.example/x"));
        // An empty policy admits any http(s) URL.
        assert!(DomainPolicy::default().admits("https://anything.example"));
        assert!(!DomainPolicy::default().admits("mailto:someone@example.com"));
    }

    #[test]
    fn harvest_continues_when_every_source_is_inadmissible() {
        // A fully-filtered item must not end the stream: a later admissible
        // item is still reachable (and closing early would strand the search).
        let stream = [
            xai_event(&search_item(&[
                json!({"type": "url", "url": "https://blocked.example/1"}),
            ])),
            xai_event(&search_item(&[
                json!({"type": "url", "url": "https://blocked.example/2"}),
                json!({"type": "url", "url": "https://kept.example/3"}),
            ])),
        ]
        .concat();
        let policy = DomainPolicy {
            allowed: Vec::new(),
            blocked: vec!["blocked.example".into()],
        };
        let Some(Harvested::Sources(links)) = harvest_all(&stream, policy, 23) else {
            panic!("expected the later admissible item to be harvested");
        };
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://kept.example/3");
    }

    #[test]
    fn error_message_renders_as_a_failed_search() {
        let subcall = detect(&subcall_body(true)).unwrap();
        let message = synthesize_error_message(
            "grok-4.5",
            &subcall,
            search_error::UNAVAILABLE,
            "xAI search returned no sources",
            42,
        );
        assert_eq!(message["content"][0]["type"], "server_tool_use");
        // Claude Code renders a non-array result content as
        // `Web search error: <error_code>`.
        assert_eq!(message["content"][1]["type"], "web_search_tool_result");
        assert_eq!(
            message["content"][1]["content"]["type"],
            "web_search_tool_result_error"
        );
        assert_eq!(
            message["content"][1]["content"]["error_code"],
            "unavailable"
        );
        assert!(!message["content"][1]["content"].is_array());
        assert_eq!(
            message["content"][2]["text"],
            "Web search failed: xAI search returned no sources"
        );
        // A failed search must not spend the session's WebSearch budget.
        assert!(message["usage"].get("server_tool_use").is_none());
        assert_eq!(message["usage"]["input_tokens"], 42);

        let body = message_to_sse(&message)
            .iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
            .collect::<String>();
        assert!(body.contains("web_search_tool_result_error"));
        assert!(body.contains("event: message_stop"));
    }

    #[test]
    fn output_tokens_are_estimated_from_the_upstream_text_not_the_cleaned_one() {
        // Citation markers, a `[wordlim: N]` annotation and the truncation
        // suffix are all rendering concerns; the upstream still produced the
        // original text, so the estimate must not shrink with them.
        let subcall = detect(&subcall_body(true)).unwrap();
        let raw = "Title (https://a.example)\n\u{E200}cite\u{E202}turn0search0\u{E201} \
                   [wordlim: 200] Published: today; body text.";
        let cleaned = clean_search_output(raw);
        assert!(
            cleaned.len() < raw.len(),
            "the fixture must actually shrink"
        );

        let message = synthesize_message("gpt-5.6-sol", &subcall, &[], raw, 7);
        // The pre-refactor value for this fixture; cleaning it first would
        // report 14.
        assert_eq!(message["usage"]["output_tokens"], 23);
        assert_eq!(message["usage"]["output_tokens"], raw.len() / 4);
        assert_ne!(message["usage"]["output_tokens"], cleaned.len() / 4);
        // The rendered commentary is still the cleaned text.
        assert_eq!(message["content"][2]["text"], cleaned);

        // Truncation is the same story, one order of magnitude up.
        let long = "x".repeat(MAX_OUTPUT_TEXT_CHARS * 2);
        let message = synthesize_message("gpt-5.6-sol", &subcall, &[], &long, 7);
        assert_eq!(message["usage"]["output_tokens"], long.len() / 4);

        // A failure accounts for the text it actually renders.
        let failure = synthesize_error_message(
            "grok-4.5",
            &subcall,
            search_error::UNAVAILABLE,
            "xAI search returned no sources",
            7,
        );
        let text = failure["content"][2]["text"].as_str().unwrap();
        assert_eq!(failure["usage"]["output_tokens"], text.len() / 4);
    }

    #[test]
    fn a_successful_search_counts_exactly_one_request() {
        let subcall = detect(&subcall_body(true)).unwrap();
        let message = synthesize_message("grok-4.5", &subcall, &[], "", 1);
        assert_eq!(
            message["usage"]["server_tool_use"]["web_search_requests"],
            1
        );
    }

    #[test]
    fn native_search_route_is_decided_by_the_origin_never_the_carrier() {
        use crate::config::{ModelFamily, ModelRoute};
        fn resolve<'a>(
            known: &'a [ModelRoute],
            origin: Option<&Origin>,
            carrier: &'a ModelRoute,
        ) -> Option<String> {
            native_search_route(origin, carrier, |routing_id| {
                known.iter().find(|route| route.routing_id == routing_id)
            })
            .map(|route| route.routing_id.clone())
        }
        let route = |routing_id: &str, family| ModelRoute {
            routing_id: routing_id.to_string(),
            family,
            ..Default::default()
        };
        let known = [
            route("grok-4.5", ModelFamily::Grok),
            route("claude-grok-4.5", ModelFamily::Grok),
            route("gpt-5.6-sol", ModelFamily::Gpt),
        ];
        let (grok, gpt) = (&known[0], &known[2]);
        let resolve = |origin: Option<Origin>, carrier| resolve(&known, origin.as_ref(), carrier);
        let gpt_origin = |routing_id: &str| {
            Some(Origin::Gpt {
                routing_id: routing_id.to_string(),
            })
        };
        let is = |routing_id: &str| Some(routing_id.to_string());
        // A Grok origin decides, whatever it is riding on.
        assert_eq!(resolve(gpt_origin("grok-4.5"), gpt), is("grok-4.5"));
        // The generated `claude-`-prefixed alias is gated by family, not by
        // the routing-id string.
        assert_eq!(
            resolve(gpt_origin("claude-grok-4.5"), gpt),
            is("claude-grok-4.5")
        );
        // A GPT origin never does — not even on a Grok carrier, and the
        // carrier's own arguments are left to the existing arms.
        assert_eq!(resolve(gpt_origin("gpt-5.6-sol"), grok), None);
        // An origin whose route has left the config inherits nothing.
        assert_eq!(resolve(gpt_origin("vanished"), grok), None);
        // A Claude origin never reaches xAI, even carried by a Grok route
        // (this is the state after a transient Anthropic failure).
        assert_eq!(
            resolve(
                Some(Origin::Claude {
                    model: "claude-haiku-4-5".into()
                }),
                grok
            ),
            None
        );
        // With no correlation, the carrier is the only signal there is.
        assert_eq!(resolve(None, grok), is("grok-4.5"));
        assert_eq!(resolve(None, gpt), None);
    }

    #[test]
    fn native_search_is_chosen_by_family() {
        use crate::config::ModelFamily;
        assert!(uses_native_search(ModelFamily::Grok));
        assert!(!uses_native_search(ModelFamily::Gpt));
        assert!(!uses_native_search(ModelFamily::OpenAiCompat));
    }

    #[test]
    fn alpha_search_model_pins_a_codex_slug_for_non_codex_routes() {
        // A Codex-native route addresses the backend with its own model.
        for codex in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert_eq!(alpha_search_model(codex), codex);
        }
        // Everything else must not: the endpoint is ChatGPT's Codex search
        // backend and a foreign slug has no meaning to it.
        for foreign in [
            "grok-4.5",
            "grok-4.20-0309-reasoning",
            "openai-compat--kimi-k3",
            "gpt-test",
        ] {
            assert_eq!(alpha_search_model(foreign), ALPHA_SEARCH_DEFAULT_MODEL);
        }
    }
}
