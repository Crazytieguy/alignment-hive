//! Which agent asked for a `WebSearch`. The sub-call itself always carries
//! the harness's small-fast model, so the router records each `WebSearch`
//! `tool_use` it sees streaming back to an agent, keyed by session, query and
//! domain filters, and the sub-call that follows looks its origin up by the
//! same key.

use serde_json::Value;

use super::{Subcall, string_list};

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

/// Passive observer of a streamed `/v1/messages` response, yielding
/// `WebSearch` tool invocations as they complete. Feed the raw SSE bytes in
/// arrival order; the caller must commit the yielded searches BEFORE
/// forwarding the chunk that produced them (the client can act on the
/// completing event the moment it sees it). Claude Code streams every turn
/// that declares the tool, so a buffered response is not tapped.
pub struct ToolUseSniffer {
    events: crate::sse::SseEventBuffer,
    /// `content_block` index → accumulated `input_json_delta` fragments for
    /// blocks named `WebSearch`.
    pending_inputs: std::collections::HashMap<u64, String>,
}

impl ToolUseSniffer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: crate::sse::SseEventBuffer::default(),
            pending_inputs: std::collections::HashMap::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<SniffedSearch> {
        if !self.events.push(chunk) {
            self.pending_inputs.clear();
            return Vec::new();
        }
        let Self {
            events,
            pending_inputs,
        } = self;
        let mut found = Vec::new();
        // The closure never returns `Some`, so every buffered event is seen.
        events.drain(|event| {
            let data = serde_json::from_str::<Value>(&crate::sse::parse_event(event)?.data).ok()?;
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
}

/// Which model asked for a pending web search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Any routed model — GPT, Grok, Muse, or open-weights — by its routing
    /// ID.
    Gpt {
        routing_id: String,
    },
    Claude {
        model: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PendingKey {
    pub session_id: String,
    pub query: String,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
}

impl PendingKey {
    /// The key a sub-call is looked up by; must derive exactly as
    /// [`Self::for_sniffed`] does or the two sides never match.
    #[must_use]
    pub fn for_subcall(session_id: String, subcall: &Subcall) -> Self {
        Self::new(
            session_id,
            &subcall.query,
            subcall.allowed_domains.as_deref(),
            subcall.blocked_domains.as_deref(),
        )
    }

    /// The key an observed `tool_use` is recorded under.
    #[must_use]
    pub fn for_sniffed(session_id: String, search: &SniffedSearch) -> Self {
        Self::new(
            session_id,
            &search.query,
            search.allowed_domains.as_deref(),
            search.blocked_domains.as_deref(),
        )
    }

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
pub(super) const MAX_PENDING_TOTAL: usize = 256;

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
