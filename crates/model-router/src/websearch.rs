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
//! This module recognizes that sub-call shape and answers it from Codex's
//! own search backend (`/v1/alpha/search`, the endpoint the Codex CLI's
//! `web.run` tool uses), which returns structured results in ~1–3s. When
//! that fails, the legacy LLM path is used with links scraped from the
//! response text into the empty result blocks.

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

/// Overall cap on bytes buffered per tapped response; overflowing disables
/// sniffing for that response (forwarding is unaffected).
const MAX_SNIFF_BUFFER_BYTES: usize = 2 * 1024 * 1024;
/// Cap on one accumulated `tool_use` input.
const MAX_SNIFF_INPUT_BYTES: usize = 64 * 1024;

/// Passive observer of a `/v1/messages` response, yielding `WebSearch` tool
/// invocations as they complete. Feed the raw response bytes in arrival
/// order; the caller must commit the yielded searches BEFORE forwarding the
/// chunk that produced them (the client can act on the completing event the
/// moment it sees it).
pub struct ToolUseSniffer {
    sse: bool,
    buffer: Vec<u8>,
    /// SSE: `content_block` index → accumulated `input_json_delta` fragments
    /// for blocks named `WebSearch`.
    pending_inputs: std::collections::HashMap<u64, String>,
    disabled: bool,
}

impl ToolUseSniffer {
    #[must_use]
    pub fn new(sse: bool) -> Self {
        Self {
            sse,
            buffer: Vec::new(),
            pending_inputs: std::collections::HashMap::new(),
            disabled: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<SniffedSearch> {
        if self.disabled {
            return Vec::new();
        }
        if self.buffer.len().saturating_add(chunk.len()) > MAX_SNIFF_BUFFER_BYTES {
            self.disabled = true;
            self.buffer = Vec::new();
            self.pending_inputs.clear();
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        if self.sse {
            self.drain_sse_events()
        } else {
            self.try_parse_json()
        }
    }

    fn drain_sse_events(&mut self) -> Vec<SniffedSearch> {
        let mut found = Vec::new();
        while let Some(event_end) = crate::usage::event_boundary(&self.buffer) {
            let event = self.buffer.drain(..event_end).collect::<Vec<_>>();
            let Ok(text) = std::str::from_utf8(&event) else {
                continue;
            };
            let Some((_, data_range)) = crate::usage::event_fields(text) else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&text[data_range]) else {
                continue;
            };
            let index = data.get("index").and_then(Value::as_u64);
            match data.get("type").and_then(Value::as_str) {
                Some("content_block_start") => {
                    let Some(block) = data.get("content_block") else {
                        continue;
                    };
                    if block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && block.get("name").and_then(Value::as_str) == Some("WebSearch")
                        && let Some(index) = index
                    {
                        self.pending_inputs.insert(index, String::new());
                    }
                }
                Some("content_block_delta") => {
                    if let Some(index) = index
                        && let Some(accumulated) = self.pending_inputs.get_mut(&index)
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
                            self.pending_inputs.remove(&index);
                        } else {
                            accumulated.push_str(fragment);
                        }
                    }
                }
                Some("content_block_stop") => {
                    if let Some(index) = index
                        && let Some(accumulated) = self.pending_inputs.remove(&index)
                        && let Ok(input) = serde_json::from_str::<Value>(&accumulated)
                        && let Some(search) = SniffedSearch::from_input(&input)
                    {
                        found.push(search);
                    }
                }
                _ => {}
            }
        }
        found
    }

    fn try_parse_json(&mut self) -> Vec<SniffedSearch> {
        // Trailing whitespace after the closing brace is valid JSON framing.
        let last_meaningful = self
            .buffer
            .iter()
            .rev()
            .find(|byte| !byte.is_ascii_whitespace());
        if last_meaningful != Some(&b'}') {
            return Vec::new();
        }
        let Ok(document) = serde_json::from_slice::<Value>(&self.buffer) else {
            return Vec::new();
        };
        self.disabled = true;
        self.buffer = Vec::new();
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
            "content": links.iter().map(Link::block).collect::<Vec<_>>(),
        }),
    ];
    let text = clean_search_output(output_text);
    if !text.trim().is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    let output_tokens = (output_text.len() / 4).max(1);
    json!({
        "id": "msg_model_router_websearch",
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "server_tool_use": {"web_search_requests": 1},
        },
    })
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
        let big = vec![b'x'; MAX_SNIFF_BUFFER_BYTES + 1];
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
}
