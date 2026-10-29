//! The response side: a stage of its own SSE framing (it must never latch
//! off silently the way the passive sniffers do) that passes events through
//! as they arrive, starts one classification per `tool_use` the moment its
//! block completes, holds `message_delta` and everything after it, and
//! injects `safeguard_results` into that first `message_delta.delta` once
//! the stream has ended. Pings keep Claude Code's 180 s idle timer from
//! firing meanwhile. A non-streaming reply gets the field at the top level.
//! Both paths settle their calls through one [`Calls`].
//!
//! Whatever an upstream sends as `safeguard_results` is dropped, so nothing
//! but this stage can settle Claude Code's verdict (the first settle wins).
//!
//! Staying out of a reply (adding nothing) is decided as early as possible:
//! the held events go out at once and running classifications are
//! cancelled. On a reply with tool calls Claude Code then keeps the whole
//! session on its own classifier.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::{Value, json};
use tokio::task::{AbortHandle, JoinSet};

use super::Judge;
use super::classify::CallVerdict;
use super::repair::{Repair, repair};
use super::transcript::{action_line, oversized_call_line};
use crate::sse::{SseFramer, parse_event, splice_data};
use crate::websearch::contains;

/// What the stage does to one reply.
#[derive(Clone, Debug)]
pub(crate) enum ReplyMode {
    /// Remove upstream `safeguard_results`; add nothing.
    StripOnly,
    /// Answer `unsupported` (a thread continuation).
    Unsupported,
    /// Judge every tool call.
    Judge(Arc<Judge>),
}

/// Bound on buffered tool input across a reply; past it the reply fails.
const MAX_TOOL_INPUT_BYTES: usize = 8 * 1024 * 1024;
/// Bound on one unterminated event; past it the stage gives up framing.
const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// Bound on what is held after `message_delta`.
const MAX_HELD_BYTES: usize = 1024 * 1024;
const PING: &[u8] = b"event: ping\ndata: {\"type\": \"ping\"}\n\n";
/// Ends a judged reply the stage can no longer frame: Claude Code retries a
/// stream error that names `overloaded_error`, and discards what it had of
/// the reply.
const RETRY_EVENT: &[u8] = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\
\"overloaded_error\",\"message\":\"model-router: the auto-mode classifier could not read this \
reply; retry the request\"}}\n\n";

/// What a reply's one classification task reports.
pub(super) enum TaskOutcome {
    Verdict(CallVerdict),
    /// The reply is left alone, for the logged reason.
    StayOut(String),
}

fn results_json(status: &Value) -> Value {
    json!([{"type": "dangerous_tool_use", "status": status}])
}

fn available(verdicts: &[(String, CallVerdict)]) -> Value {
    let tool_uses: serde_json::Map<String, Value> = verdicts
        .iter()
        .map(|(id, verdict)| (id.clone(), verdict.to_json()))
        .collect();
    results_json(&json!({"type": "available", "tool_uses": tool_uses}))
}

fn unsupported() -> Value {
    results_json(&json!({"type": "unsupported"}))
}

/// An MCP tool's server key, normalised the way Claude Code tells its
/// servers apart: lower case, `_` and spaces as `-`, and the `-a<8 hex>`
/// suffix it adds to a colliding key removed.
fn mcp_server(name: &str) -> Option<String> {
    let server = name.strip_prefix("mcp__")?.split("__").next()?;
    let mut server: String = server
        .chars()
        .map(|c| match c {
            '_' | ' ' => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    if let Some(at) = server.rfind("-a")
        && server.len() == at + 10
        && server[at + 2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        server.truncate(at);
    }
    Some(server)
}

/// Tools whose safe use turns on client state the router does not render
/// (artifact sharing, browser tabs, the memory store): a reply with one is
/// left to Claude Code's own classifier.
pub(super) fn needs_client_state(name: &str) -> bool {
    if matches!(
        name,
        "Artifact"
            | "ArtifactComments"
            | "ArtifactData"
            | "ArtifactCheck"
            | "memory_list"
            | "memory_read"
            | "memory_write"
    ) {
        return true;
    }
    let lowered = name.to_ascii_lowercase();
    if lowered.contains("claude_browser") || lowered.contains("claude-browser") {
        return true;
    }
    let tool = lowered.rsplit("__").next().unwrap_or_default();
    match mcp_server(name).as_deref() {
        Some("claude-in-chrome") => true,
        Some("claude-device") => tool.starts_with("chrome_"),
        _ => false,
    }
}

/// Watches bytes that pass through unframed for an upstream verdict, which
/// must never reach the client: once seen, the reply is cut short there.
/// Only for replies the router does not judge; a judged reply that cannot
/// be framed or buffered is ended for the client to retry instead.
#[derive(Default)]
struct RawGuard {
    tail: Vec<u8>,
}

const VERDICT_FIELD: &[u8] = b"safeguard_results";

impl RawGuard {
    /// Whether `chunk` may pass.
    fn admit(&mut self, chunk: &[u8]) -> bool {
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        if contains(&window, VERDICT_FIELD) {
            tracing::warn!(
                "auto-mode classifier: an unframed reply carried safeguard_results; cutting it \
                 short"
            );
            return false;
        }
        let keep = window.len().min(VERDICT_FIELD.len() - 1);
        self.tail = window.split_off(window.len() - keep);
        true
    }
}

/// Whether an event's bytes could decode to a `safeguard_results` key:
/// spelled out, or behind a `\u` escape.
fn may_carry_verdict(event: &[u8]) -> bool {
    contains(event, VERDICT_FIELD) || contains(event, b"\\u")
}

/// A pass-through body that ends early rather than carry an upstream
/// `safeguard_results` (the buffered path's overflow, on a reply the router
/// does not judge).
pub(crate) fn guard_raw(
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    Box::pin(async_stream::stream! {
        let mut inner = inner;
        let mut guard = RawGuard::default();
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) if !guard.admit(&chunk) => return,
                item => yield item,
            }
        }
    })
}

/// What became of one call.
enum State {
    /// Its input is still streaming.
    Open,
    /// Its input did not parse (cut off, or malformed).
    Unreadable,
    /// Settled without a classification.
    Fixed(CallVerdict),
    /// Being classified.
    Judging(AbortHandle),
}

struct Call {
    id: String,
    name: String,
    state: State,
}

/// One reply's calls, for the streaming stage and the JSON path alike:
/// each call is classified the moment its input is complete, with the
/// reply's earlier calls as context, and the reply settles once from them.
struct Calls {
    judge: Arc<Judge>,
    all: Vec<Call>,
    /// Lines of the reply's finished calls, in finishing order.
    earlier: Vec<Arc<str>>,
    tasks: JoinSet<(String, TaskOutcome)>,
    /// The verdicts of the classifications that finished.
    verdicts: HashMap<String, CallVerdict>,
    /// Something in the reply could not be read: every verdict is `error`.
    failed: bool,
    /// The reply is left to Claude Code (a tool that needs client state,
    /// or a classification that stayed out).
    stay_out: bool,
}

impl Calls {
    fn new(judge: Arc<Judge>) -> Self {
        Self {
            judge,
            all: Vec::new(),
            earlier: Vec::new(),
            tasks: JoinSet::new(),
            verdicts: HashMap::new(),
            failed: false,
            stay_out: false,
        }
    }

    /// Every verdict is known; nothing worth waiting for is running.
    fn settled(&self) -> bool {
        self.failed || self.stay_out || self.tasks.is_empty()
    }

    /// Every verdict is `error` now: running classifications are cancelled.
    fn fail(&mut self) {
        self.failed = true;
        self.tasks.abort_all();
    }

    fn leave(&mut self, why: &str) {
        tracing::debug!(%why, "auto-mode classifier: leaving the reply alone");
        self.stay_out = true;
        self.tasks.abort_all();
    }

    /// A new call; returns its index.
    fn open(&mut self, id: &str, name: &str) -> usize {
        if needs_client_state(name) {
            self.leave(&format!("{name} needs client state"));
        }
        // A repeated id is `error` whatever its first call's classification
        // says.
        for call in &self.all {
            if call.id == id
                && let State::Judging(task) = &call.state
            {
                task.abort();
            }
        }
        self.all.push(Call {
            id: id.to_string(),
            name: name.to_string(),
            state: State::Open,
        });
        self.all.len() - 1
    }

    /// Whether more than one call has `id`: each is then `error`.
    fn repeated(&self, id: &str) -> bool {
        self.all.iter().filter(|call| call.id == id).count() > 1
    }

    /// Settles call `index` without classifying it.
    fn fix(&mut self, index: usize, verdict: CallVerdict) {
        self.all[index].state = State::Fixed(verdict);
    }

    /// Call `index`'s input is complete (`None`: it did not parse): it is
    /// classified as Claude Code will run it, or settled.
    fn finish(&mut self, index: usize, input: Option<&Value>) {
        let Some(input) = input.filter(|input| input.is_object()) else {
            self.all[index].state = State::Unreadable;
            return;
        };
        let call = &self.all[index];
        let (id, name) = (call.id.clone(), call.name.clone());
        let input = match repair(&name, input, self.judge.tool_schema(&name)) {
            Repair::Input(input) => input,
            Repair::Unfaithful => {
                tracing::debug!(tool = %name, "auto-mode classifier: cannot reproduce Claude Code's input repair");
                self.earlier.push(shown_line(&name, input).0);
                self.fix(index, CallVerdict::Unavailable("error"));
                return;
            }
        };
        let (line, fits) = shown_line(&name, &input);
        if !fits {
            self.earlier.push(line);
            self.fix(index, CallVerdict::Unavailable("error"));
            return;
        }
        if self.failed || self.stay_out || self.repeated(&id) {
            self.fix(index, CallVerdict::Unavailable("error"));
        } else {
            // Each call's budget runs from when its own input finished.
            let deadline = tokio::time::Instant::now() + self.judge.limits().reply_budget;
            let (judge, earlier, action) = (self.judge.clone(), self.earlier.clone(), line.clone());
            let task = self.tasks.spawn(async move {
                let outcome = judge.judge(&id, &name, &earlier, &action, deadline).await;
                (id, outcome)
            });
            self.all[index].state = State::Judging(task);
        }
        self.earlier.push(line);
    }

    /// Waits for the next classification; `false` when none is running.
    async fn join_next(&mut self) -> bool {
        match self.tasks.join_next().await {
            Some(Ok((id, outcome))) => {
                self.record(id, outcome);
                true
            }
            // A lost or cancelled task leaves its id without an outcome.
            Some(Err(_)) => true,
            None => false,
        }
    }

    fn record(&mut self, id: String, outcome: TaskOutcome) {
        match outcome {
            TaskOutcome::StayOut(why) => self.leave(&why),
            TaskOutcome::Verdict(verdict) => {
                self.verdicts.insert(id, verdict);
            }
        }
    }

    /// The reply's `safeguard_results`, or `None` to leave it alone.
    /// `truncated`: the reply stopped at its output limit.
    fn settle(&mut self, truncated: bool) -> Option<Value> {
        if self.stay_out {
            return None;
        }
        let mut seen = HashSet::new();
        let mut verdicts = Vec::new();
        for call in &self.all {
            if !seen.insert(call.id.as_str()) {
                continue;
            }
            let verdict = if self.failed || self.repeated(&call.id) {
                CallVerdict::Unavailable("error")
            } else {
                match &call.state {
                    // Cut off by the output limit: Claude Code asks for the
                    // call again once. Otherwise unreadable.
                    State::Open | State::Unreadable => {
                        CallVerdict::Unavailable(if truncated { "truncated" } else { "error" })
                    }
                    State::Fixed(verdict) => verdict.clone(),
                    State::Judging(_) => self
                        .verdicts
                        .remove(&call.id)
                        .unwrap_or(CallVerdict::Unavailable("error")),
                }
            };
            verdicts.push((call.id.clone(), verdict));
        }
        if self.failed && verdicts.is_empty() {
            return Some(results_json(
                &json!({"type": "unavailable", "reason": "error"}),
            ));
        }
        Some(available(&verdicts))
    }
}

/// A call's line for the classifier, and whether it fits: one too large
/// to show is a placeholder.
fn shown_line(name: &str, input: &Value) -> (Arc<str>, bool) {
    let line = action_line(name, input);
    if line.len() > super::MAX_ACTION_BYTES {
        (oversized_call_line(name, line.len()).into(), false)
    } else {
        (line.into(), true)
    }
}

/// One streamed `tool_use` block.
struct Block {
    /// Its index in [`Calls`].
    call: usize,
    start_input: Value,
    fragments: String,
    /// Its input cannot be read as sent.
    broken: bool,
}

/// A `message_delta`: the event as it came and its payload, re-serialized
/// only once something in it changed.
struct HeldDelta {
    original: Vec<u8>,
    /// Where the payload sits, when it is one `data:` line.
    range: Option<std::ops::Range<usize>>,
    data: Value,
    modified: bool,
}

impl HeldDelta {
    fn set_results(&mut self, results: Value) {
        if let Some(delta) = self.data.get_mut("delta").and_then(Value::as_object_mut) {
            delta.insert("safeguard_results".to_string(), results);
            self.modified = true;
        }
    }

    /// The event: as it came, the payload spliced back in, or rebuilt as one
    /// `data:` line when the payload spanned several.
    fn render(self) -> Bytes {
        if !self.modified {
            return Bytes::from(self.original);
        }
        let data = self.data.to_string();
        Bytes::from(match &self.range {
            Some(range) => splice_data(&self.original, range, data.as_bytes()),
            None => format!("event: message_delta\ndata: {data}\n\n").into_bytes(),
        })
    }
}

struct Stage {
    /// The reply's calls, tracked only while judging.
    calls: Option<Calls>,
    /// The first `message_delta` gets `unsupported`.
    unsupported: bool,
    framer: SseFramer,
    /// Framing was abandoned on a reply not judged; bytes pass through raw
    /// from here, guarded.
    framing_lost: Option<RawGuard>,
    /// The reply ends here: an upstream verdict surfaced in raw bytes, or a
    /// judged reply could not be framed.
    cut: bool,
    blocks: HashMap<u64, Block>,
    tool_input_bytes: usize,
    delta_seen: bool,
    stop_reason: Option<String>,
    held_delta: Option<HeldDelta>,
    held: Vec<Bytes>,
    held_bytes: usize,
}

impl Stage {
    fn new(mode: ReplyMode) -> Self {
        let (calls, unsupported) = match mode {
            ReplyMode::Judge(judge) => (Some(Calls::new(judge)), false),
            ReplyMode::Unsupported => (None, true),
            ReplyMode::StripOnly => (None, false),
        };
        Self {
            calls,
            unsupported,
            framer: SseFramer::default(),
            framing_lost: None,
            cut: false,
            blocks: HashMap::new(),
            tool_input_bytes: 0,
            delta_seen: false,
            stop_reason: None,
            held_delta: None,
            held: Vec::new(),
            held_bytes: 0,
        }
    }

    const fn holding(&self) -> bool {
        self.held_delta.is_some()
    }

    fn truncated(&self) -> bool {
        self.stop_reason.as_deref() == Some("max_tokens")
    }

    fn fail(&mut self) {
        if let Some(calls) = &mut self.calls {
            calls.fail();
        }
    }

    /// Stops judging: the classifications are cancelled and what was held
    /// goes out as it came.
    fn stop_judging(&mut self) -> Vec<Bytes> {
        self.calls = None;
        self.release_held()
    }

    /// Settles the reply's calls into the held `message_delta`.
    fn inject_results(&mut self) {
        let truncated = self.truncated();
        if let Some(delta) = &mut self.held_delta
            && let Some(results) = self
                .calls
                .as_mut()
                .and_then(|calls| calls.settle(truncated))
        {
            delta.set_results(results);
        }
    }

    /// Gives the reply up: the classifications are cancelled (dropping the
    /// tasks aborts them) and nothing held or buffered goes out.
    fn end_for_retry(&mut self) {
        self.calls = None;
        self.held_delta = None;
        self.held.clear();
        self.held_bytes = 0;
        self.framer.take_rest();
        self.cut = true;
    }

    /// At the end of the stream: a last event that lacks its blank line is
    /// still an event, read like the others (never passed on raw).
    fn flush_tail(&mut self) -> Vec<Bytes> {
        let mut rest = self.framer.take_rest();
        if rest.iter().all(u8::is_ascii_whitespace) {
            return if rest.is_empty() {
                Vec::new()
            } else {
                vec![Bytes::from(rest)]
            };
        }
        rest.extend_from_slice(b"\n\n");
        let mut out = Vec::new();
        self.take_event(rest, &mut out);
        out
    }

    fn push(&mut self, chunk: &Bytes) -> Vec<Bytes> {
        if let Some(guard) = &mut self.framing_lost {
            if guard.admit(chunk) {
                return vec![chunk.clone()];
            }
            self.cut = true;
            return Vec::new();
        }
        self.framer.push(chunk);
        let mut out = Vec::new();
        while let Some(event) = self.framer.next_event() {
            self.take_event(event, &mut out);
        }
        if self.framer.buffered() > MAX_EVENT_BYTES {
            if self.calls.is_some() {
                // Raw bytes cannot be judged, nor shown to be free of an
                // upstream verdict: the reply ends here and is asked again.
                tracing::warn!(
                    "auto-mode classifier: an SSE event outgrew the bound; ending the reply for \
                     the client to retry"
                );
                self.end_for_retry();
                out.push(Bytes::from_static(RETRY_EVENT));
                return out;
            }
            tracing::warn!("auto-mode classifier: an SSE event outgrew the bound");
            out.extend(self.stop_judging());
            let mut guard = RawGuard::default();
            let rest = self.framer.take_rest();
            if guard.admit(&rest) {
                out.push(Bytes::from(rest));
            } else {
                self.cut = true;
            }
            self.framing_lost = Some(guard);
        }
        out
    }

    /// Processes one complete event into `out`, along with everything held
    /// should the reply be left alone from here.
    fn take_event(&mut self, event: Vec<u8>, out: &mut Vec<Bytes>) {
        out.extend(self.event(event));
        if self.calls.as_ref().is_some_and(|calls| calls.stay_out) {
            out.extend(self.stop_judging());
        }
    }

    /// Processes one complete event; returns it (maybe rewritten) unless
    /// it is held.
    fn event(&mut self, event: Vec<u8>) -> Option<Bytes> {
        // With nothing to judge or inject, only an upstream verdict needs
        // the payload read.
        if self.calls.is_none() && !self.unsupported && !may_carry_verdict(&event) {
            return self.emit(Bytes::from(event));
        }
        let Ok(text) = std::str::from_utf8(&event) else {
            self.fail();
            return self.emit(Bytes::from(event));
        };
        let Some(parsed) = parse_event(text) else {
            return self.emit(Bytes::from(event));
        };
        let name = parsed.name.unwrap_or_default().to_string();
        let Ok(data) = serde_json::from_str::<Value>(&parsed.data) else {
            if !matches!(name.as_str(), "" | "ping") {
                self.fail();
            }
            return self.emit(Bytes::from(event));
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(&name)
            .to_string();
        match kind.as_str() {
            "content_block_start" => self.block_start(&data),
            "content_block_delta" => self.block_delta(&data),
            "content_block_stop" => self.block_stop(&data),
            "message_delta" => {
                let range = parsed.data_range.clone();
                return self.message_delta(event, range, data);
            }
            _ => {}
        }
        self.emit(Bytes::from(event))
    }

    fn emit(&mut self, bytes: Bytes) -> Option<Bytes> {
        if !self.holding() {
            return Some(bytes);
        }
        self.held_bytes += bytes.len();
        self.held.push(bytes);
        if self.held_bytes > MAX_HELD_BYTES {
            // Nothing sane follows `message_delta` at this size: settle the
            // reply now, failed, and stop holding.
            self.fail();
            self.inject_results();
            return Some(Bytes::from(self.stop_judging().concat()));
        }
        None
    }

    fn block_start(&mut self, data: &Value) {
        let delta_seen = self.delta_seen;
        let Some(calls) = &mut self.calls else {
            return;
        };
        if delta_seen {
            calls.fail();
        }
        let block = data.get("content_block").unwrap_or(&Value::Null);
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return;
        }
        let (Some(index), Some(id)) = (
            data.get("index").and_then(Value::as_u64),
            block
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty()),
        ) else {
            calls.fail();
            return;
        };
        if self.blocks.contains_key(&index) {
            calls.fail();
            return;
        }
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let call = calls.open(id, name);
        self.blocks.insert(
            index,
            Block {
                call,
                start_input: block.get("input").cloned().unwrap_or(Value::Null),
                fragments: String::new(),
                broken: false,
            },
        );
    }

    fn block_delta(&mut self, data: &Value) {
        if self.calls.is_none() {
            return;
        }
        let delta = data.get("delta").unwrap_or(&Value::Null);
        if delta.get("type").and_then(Value::as_str) != Some("input_json_delta") {
            return;
        }
        let Some(index) = data
            .get("index")
            .and_then(Value::as_u64)
            .filter(|index| self.blocks.contains_key(index))
        else {
            return;
        };
        let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) else {
            if let Some(block) = self.blocks.get_mut(&index) {
                block.broken = true;
            }
            return;
        };
        self.tool_input_bytes += fragment.len();
        if self.tool_input_bytes > MAX_TOOL_INPUT_BYTES {
            self.fail();
            return;
        }
        if let Some(block) = self.blocks.get_mut(&index) {
            block.fragments.push_str(fragment);
        }
    }

    fn block_stop(&mut self, data: &Value) {
        let Some(calls) = &mut self.calls else {
            return;
        };
        let Some(block) = data
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| self.blocks.get_mut(&index))
        else {
            return;
        };
        let fragments = std::mem::take(&mut block.fragments);
        let start_input = std::mem::take(&mut block.start_input);
        if block.broken {
            calls.fix(block.call, CallVerdict::Unavailable("error"));
            return;
        }
        let start_empty = match &start_input {
            Value::Null => true,
            Value::Object(object) => object.is_empty(),
            _ => false,
        };
        // Claude Code takes the concatenated fragments when there are any,
        // else the start block's input; both at once is ambiguous.
        let input = if fragments.is_empty() {
            Some(match start_input {
                Value::Null => json!({}),
                input => input,
            })
        } else if start_empty {
            serde_json::from_str::<Value>(&fragments).ok()
        } else {
            calls.fix(block.call, CallVerdict::Unavailable("error"));
            return;
        };
        calls.finish(block.call, input.as_ref());
    }

    fn message_delta(
        &mut self,
        event: Vec<u8>,
        range: Option<std::ops::Range<usize>>,
        mut data: Value,
    ) -> Option<Bytes> {
        let stripped = data
            .get_mut("delta")
            .and_then(Value::as_object_mut)
            .and_then(|delta| delta.remove("safeguard_results"))
            .is_some();
        let first = !self.delta_seen;
        self.delta_seen = true;
        if first {
            self.stop_reason = data
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        let mut held = HeldDelta {
            original: event,
            range,
            data,
            modified: stripped,
        };
        if first && self.calls.is_some() {
            self.held_delta = Some(held);
            return None;
        }
        if first && self.unsupported {
            held.set_results(unsupported());
        }
        self.emit(held.render())
    }

    /// The held events, unchanged but for the injection already made.
    fn release_held(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        if let Some(delta) = self.held_delta.take() {
            out.push(delta.render());
        }
        out.append(&mut self.held);
        self.held_bytes = 0;
        out
    }

    /// Waits until every verdict is known; `true` when a ping is due first.
    async fn wait(&mut self, ping_at: &mut tokio::time::Instant, interval: Duration) -> bool {
        let Some(calls) = &mut self.calls else {
            return false;
        };
        while !calls.settled() {
            tokio::select! {
                running = calls.join_next() => if !running {
                    break;
                },
                () = tokio::time::sleep_until(*ping_at) => {
                    *ping_at += interval;
                    return true;
                }
            }
        }
        false
    }

    /// Everything still owed to the client once the classifications are in.
    fn finish(&mut self) -> Vec<Bytes> {
        let rest = self.framer.take_rest();
        self.inject_results();
        self.calls = None;
        let mut out = self.release_held();
        if !rest.is_empty() {
            out.push(Bytes::from(rest));
        }
        out
    }
}

/// Wraps a 200 SSE reply in the stage.
pub(crate) fn sse(
    mode: ReplyMode,
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    Box::pin(async_stream::stream! {
        let mut inner = inner;
        let mut stage = Stage::new(mode);
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) => {
                    for bytes in stage.push(&chunk) {
                        yield Ok(bytes);
                    }
                    if stage.cut {
                        return;
                    }
                }
                Err(error) => {
                    // A broken stream settles as `server_stream_ended` on the
                    // client; no verdict is injected into it.
                    for bytes in stage.stop_judging() {
                        yield Ok(bytes);
                    }
                    yield Err(error);
                    return;
                }
            }
        }
        for bytes in stage.flush_tail() {
            yield Ok(bytes);
        }
        let interval = stage
            .calls
            .as_ref()
            .filter(|_| stage.holding())
            .map(|calls| calls.judge.limits().ping_interval);
        if let Some(interval) = interval {
            let mut ping_at = tokio::time::Instant::now() + interval;
            while stage.wait(&mut ping_at, interval).await {
                yield Ok(Bytes::from_static(PING));
            }
        }
        for bytes in stage.finish() {
            yield Ok(bytes);
        }
    })
}

/// Rewrites a buffered 200 JSON reply; `None` leaves it as it came.
pub(crate) async fn json(mode: &ReplyMode, body: &[u8]) -> Option<Vec<u8>> {
    let mut message: Value = serde_json::from_slice(body).ok()?;
    let object = message.as_object_mut()?;
    let stripped = object.remove("safeguard_results").is_some();
    let results = match mode {
        ReplyMode::StripOnly => None,
        ReplyMode::Unsupported => Some(unsupported()),
        ReplyMode::Judge(judge) => judge_message(judge, &message).await,
    };
    match results {
        Some(results) => {
            message["safeguard_results"] = results;
            Some(message.to_string().into_bytes())
        }
        None => stripped.then(|| message.to_string().into_bytes()),
    }
}

async fn judge_message(judge: &Arc<Judge>, message: &Value) -> Option<Value> {
    let blocks: Vec<&Value> = message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .collect();
    let name = |block: &Value| {
        block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    // Known before anything runs: no classification is spent on it.
    if blocks.iter().any(|block| needs_client_state(&name(block))) {
        return None;
    }
    let mut calls = Calls::new(judge.clone());
    for block in blocks {
        match block
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(id) => {
                let index = calls.open(id, &name(block));
                calls.finish(index, block.get("input"));
            }
            None => calls.fail(),
        }
    }
    while !calls.settled() && calls.join_next().await {}
    let truncated = message.get("stop_reason").and_then(Value::as_str) == Some("max_tokens");
    calls.settle(truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(data: &Value) -> Vec<u8> {
        format!(
            "event: {}\ndata: {data}\n\n",
            data["type"].as_str().unwrap()
        )
        .into_bytes()
    }

    fn judging() -> Stage {
        Stage::new(ReplyMode::Judge(crate::auto_mode::test_judge(&[])))
    }

    fn settle(stage: &mut Stage) -> Value {
        let truncated = stage.truncated();
        stage.calls.as_mut().unwrap().settle(truncated).unwrap()
    }

    #[test]
    fn strip_only_removes_upstream_results_and_passes_the_rest_byte_for_byte() {
        let mut stage = Stage::new(ReplyMode::StripOnly);
        let start = event(&json!({"type": "message_start", "message": {"id": "m"}}));
        let delta = event(&json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn", "safeguard_results": [{"type": "dangerous_tool_use",
                "status": {"type": "available", "tool_uses": {"x": {"type": "evaluated", "outcome": "not_flagged"}}}}]}}));
        let mut out = stage.push(&Bytes::from(start.clone()));
        out.extend(stage.push(&Bytes::from(delta)));
        assert_eq!(out[0], start);
        let text = String::from_utf8(out[1].to_vec()).unwrap();
        assert!(!text.contains("safeguard_results"), "{text}");
        assert!(text.contains(r#""stop_reason":"end_turn""#));

        // A key spelled with an escape decodes to the same field.
        let escaped = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\
            \"safeguard\\u005fresults\":[]}}\n\n";
        let out = stage.push(&Bytes::from_static(escaped));
        assert_eq!(
            out[0],
            &b"event: message_delta\ndata: {\"delta\":{},\"type\":\"message_delta\"}\n\n"[..]
        );
    }

    #[test]
    fn strip_only_tracks_no_calls() {
        let mut stage = Stage::new(ReplyMode::StripOnly);
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "a", "name": "Bash", "input": {}}}),
        )));
        assert!(stage.calls.is_none() && stage.blocks.is_empty());
    }

    #[test]
    fn unsupported_is_injected_into_the_first_message_delta_only() {
        let mut stage = Stage::new(ReplyMode::Unsupported);
        let delta = event(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));
        let first = stage.push(&Bytes::from(delta.clone()));
        let second = stage.push(&Bytes::from(delta.clone()));
        assert!(String::from_utf8_lossy(&first[0]).contains(
            r#""safeguard_results":[{"status":{"type":"unsupported"},"type":"dangerous_tool_use"}]"#
        ));
        assert_eq!(second[0], delta);
    }

    /// A payload over several `data:` lines is rebuilt as one.
    #[test]
    fn a_multi_line_message_delta_is_rebuilt_with_the_results() {
        let mut stage = Stage::new(ReplyMode::Unsupported);
        let out = stage.push(&Bytes::from_static(
            b"event: message_delta\ndata: {\"type\": \"message_delta\",\ndata: \"delta\": {\"stop_reason\": \"end_turn\"}}\n\n",
        ));
        let text = String::from_utf8(out[0].to_vec()).unwrap();
        assert!(text.starts_with("event: message_delta\ndata: {"), "{text}");
        assert!(
            text.ends_with("}\n\n") && text.matches("data:").count() == 1,
            "{text}"
        );
        let data: Value = serde_json::from_str(
            text.trim_end()
                .strip_prefix("event: message_delta\ndata: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(data["delta"]["stop_reason"], "end_turn");
        assert_eq!(data["delta"]["safeguard_results"], unsupported());
    }

    #[tokio::test]
    async fn cut_off_conflicting_and_duplicate_calls_never_reach_the_classifier() {
        let mut stage = judging();
        let events = [
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "x"}}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{\"command\":\"y\"}"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "b", "name": "Bash", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"comm"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2,
                "content_block": {"type": "tool_use", "id": "c", "name": "Bash", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "input_json_delta", "partial_json": "{\"command\":"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
        ];
        for data in &events {
            stage.push(&Bytes::from(event(data)));
        }
        let results = settle(&mut stage);
        let tool_uses = &results[0]["status"]["tool_uses"];
        assert_eq!(tool_uses["a"]["reason"], "error", "both inputs: ambiguous");
        assert_eq!(tool_uses["b"]["reason"], "truncated", "closed but cut off");
        assert_eq!(tool_uses["c"]["reason"], "truncated", "never closed");
        assert!(stage.calls.as_ref().unwrap().tasks.is_empty());

        let mut stage = judging();
        for (index, id) in ["d", "d"].iter().enumerate() {
            stage.push(&Bytes::from(event(
                &json!({"type": "content_block_start", "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": "Read",
                    "input": {"file_path": "/x"}}}),
            )));
            stage.push(&Bytes::from(event(
                &json!({"type": "content_block_stop", "index": index}),
            )));
        }
        let results = settle(&mut stage);
        assert_eq!(
            results[0]["status"]["tool_uses"]["d"]["reason"], "error",
            "duplicate id"
        );

        // A tool block after the message delta fails the whole reply.
        let mut stage = judging();
        stage.push(&Bytes::from(event(
            &json!({"type": "message_delta", "delta": {}}),
        )));
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "e", "name": "Bash", "input": {}}}),
        )));
        assert_eq!(
            settle(&mut stage)[0]["status"]["tool_uses"]["e"]["reason"],
            "error"
        );
        let mut stage = judging();
        stage.push(&Bytes::from_static(
            b"event: message_delta\ndata: {not json\n\n",
        ));
        assert_eq!(
            settle(&mut stage)[0]["status"],
            json!({"type": "unavailable", "reason": "error"})
        );
    }

    #[tokio::test]
    async fn an_input_claude_code_would_coerce_is_an_error_and_an_oversized_one_a_placeholder() {
        let schema = json!({"type": "object", "properties": {"timeout": {"type": "number"}}});
        let mut stage = Stage::new(ReplyMode::Judge(crate::auto_mode::test_judge(&[(
            "Bash", schema,
        )])));
        let big = "x".repeat(super::super::MAX_ACTION_BYTES + 1);
        for (index, input) in [
            json!({"command": "ls", "timeout": "120"}),
            json!({"command": big}),
        ]
        .iter()
        .enumerate()
        {
            stage.push(&Bytes::from(event(&json!({"type": "content_block_start",
                "index": index, "content_block": {"type": "tool_use", "id": format!("t{index}"),
                "name": "Bash", "input": input}}))));
            stage.push(&Bytes::from(event(
                &json!({"type": "content_block_stop", "index": index}),
            )));
        }
        let calls = stage.calls.as_ref().unwrap();
        assert!(calls.tasks.is_empty());
        let shown = action_line("Bash", &json!({"command": big})).len();
        assert_eq!(
            &*calls.earlier[1],
            format!(r#"{{"Bash":"[input omitted: {shown} bytes]"}}"#)
        );
        let results = settle(&mut stage);
        assert_eq!(results[0]["status"]["tool_uses"]["t0"]["reason"], "error");
        assert_eq!(results[0]["status"]["tool_uses"]["t1"]["reason"], "error");
    }

    /// A client-state tool ends judging at once: the classifications already
    /// running are cancelled and a held delta goes out unchanged.
    #[tokio::test]
    async fn a_client_state_tool_stops_judging_at_once() {
        let mut stage = judging();
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "a", "name": "Bash",
                "input": {"command": "ls"}}}),
        )));
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_stop", "index": 0}),
        )));
        assert_eq!(stage.calls.as_ref().unwrap().tasks.len(), 1);
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 1,
            "content_block": {"type": "tool_use", "id": "b",
                "name": "mcp__Claude-In-Chrome-a1b2c3d4e__navigate", "input": {}}}),
        )));
        assert!(stage.calls.is_none());
        let delta = event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));
        let out = stage.push(&Bytes::from(delta.clone()));
        assert_eq!(out, [delta]);
        assert!(!stage.holding());
    }

    /// More than the bound held after `message_delta` settles the reply as
    /// failed and releases it.
    #[tokio::test]
    async fn an_oversized_hold_settles_the_reply_as_failed() {
        let mut stage = judging();
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "a", "name": "Bash",
                "input": {"command": "ls"}}}),
        )));
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_stop", "index": 0}),
        )));
        let delta = event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));
        assert_eq!(stage.push(&Bytes::from(delta)).len(), 0, "held");
        let filler = format!(": {}\n\n", "x".repeat(MAX_HELD_BYTES));
        let out = stage.push(&Bytes::from(filler.clone()));
        assert_eq!(out.len(), 1);
        let text = String::from_utf8(out[0].to_vec()).unwrap();
        assert!(text.ends_with(&filler));
        assert!(
            text.contains(r#""tool_uses":{"a":{"reason":"error","type":"unavailable"}}"#),
            "{}",
            &text[..300]
        );
        assert!(stage.calls.is_none() && !stage.holding());
    }

    /// Once framing is lost, raw bytes pass until one carries an upstream
    /// verdict, where the reply is cut short.
    #[test]
    fn raw_bytes_are_cut_at_an_upstream_verdict() {
        let mut stage = Stage::new(ReplyMode::StripOnly);
        let huge = Bytes::from(vec![b'x'; MAX_EVENT_BYTES + 1]);
        assert_eq!(stage.push(&huge), std::slice::from_ref(&huge));
        assert!(stage.framing_lost.is_some() && !stage.cut);
        assert_eq!(stage.push(&Bytes::from_static(b"more")).len(), 1);
        // Split across two chunks.
        assert_eq!(stage.push(&Bytes::from_static(b"\"safeguard")).len(), 1);
        assert_eq!(stage.push(&Bytes::from_static(b"_results\":[]")).len(), 0);
        assert!(stage.cut);
    }

    /// A judged reply that outgrows the framing bound is never passed on
    /// raw: its classifications are cancelled, nothing held goes out, and
    /// the reply ends with an error Claude Code retries.
    #[tokio::test]
    async fn a_judged_reply_that_cannot_be_framed_ends_for_a_retry() {
        let mut stage = judging();
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "a", "name": "Bash",
                "input": {"command": "ls"}}}),
        )));
        stage.push(&Bytes::from(event(
            &json!({"type": "content_block_stop", "index": 0}),
        )));
        let delta = event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));
        assert!(stage.push(&Bytes::from(delta)).is_empty(), "held");
        let mut huge = b"event: message_stop\ndata: {\"safeguard_results\":[]".to_vec();
        huge.resize(MAX_EVENT_BYTES + 1, b' ');
        let out = stage.push(&Bytes::from(huge));
        assert_eq!(out, [Bytes::from_static(RETRY_EVENT)]);
        assert!(stage.cut && stage.calls.is_none() && !stage.holding());
        let error: Value = serde_json::from_str(
            std::str::from_utf8(RETRY_EVENT)
                .unwrap()
                .trim_end()
                .strip_prefix("event: error\ndata: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(error["error"]["type"], "overloaded_error");
    }

    /// A last event without its blank line is read like any other, so an
    /// upstream verdict in it is stripped and a judged reply's delta held.
    #[tokio::test]
    async fn an_unterminated_last_event_is_still_read() {
        let mut stage = Stage::new(ReplyMode::StripOnly);
        let tail = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\
            \"safeguard_results\":[{\"type\":\"dangerous_tool_use\"}]}}";
        assert_eq!(stage.push(&Bytes::from_static(tail)), Vec::<Bytes>::new());
        let out = stage.flush_tail();
        assert_eq!(out.len(), 1);
        let text = String::from_utf8(out[0].to_vec()).unwrap();
        assert!(
            !text.contains("safeguard_results") && text.ends_with("\n\n"),
            "{text}"
        );

        let mut stage = judging();
        stage.push(&Bytes::from_static(
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{}}",
        ));
        assert!(stage.flush_tail().is_empty() && stage.holding());
    }

    #[test]
    fn client_state_tools_are_recognized() {
        for name in [
            "Artifact",
            "memory_write",
            "mcp__claude-in-chrome__navigate",
            "mcp__claude-in-chrome-a0b1c2d3e__navigate",
            "mcp__Claude_In_Chrome__navigate",
            "mcp__Claude_Browser__click",
            "mcp__claude-device__chrome_open",
            "mcp__Claude_Device__chrome_open",
        ] {
            assert!(needs_client_state(name), "{name}");
        }
        for name in [
            "Bash",
            "mcp__github__create_issue",
            "ArtifactX",
            "mcp__claude-device__shell",
            "mcp__claude-in-chromex__navigate",
        ] {
            assert!(!needs_client_state(name), "{name}");
        }
    }
}
