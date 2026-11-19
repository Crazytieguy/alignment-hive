//! One text block per run of text in a streamed Gemini reply.
//!
//! The managed `CLIProxyAPI` child (8.0.69) turns each text part Google
//! streams into a `text` content block of its own, and the thought
//! signature Google sends after a reply's text (in every streamed text reply
//! captured on 2026-11-18) into an empty `thinking` block after them, a
//! carrier. Claude Code makes an assistant message of each block and takes
//! a reply's text from the last one: `claude -p` printed nothing, a subagent
//! reported its last chunk, and `/compact` kept a one-chunk summary.
//!
//! [`stage`] joins a run of text blocks into one, and holds back the
//! carriers that follow it until the reply shows what comes next:
//!
//! - more text: the text joins the block, and the carriers, which signed a
//!   part of a text the client now has whole, are dropped;
//! - another block, or the end of a reply that calls a tool: the carriers
//!   go on after the text, in the place the child gave them;
//! - the end of a reply that calls no tool: the carriers are dropped, so
//!   the text is the reply's last block.
//!
//! A dropped carrier held the signature of a text, which Google does not
//! validate on replay: it answered every replay without one (2026-11-18).

use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;
use serde_json::{Value, json};

use crate::sse::{SseFramer, parse_event};

/// Wraps a 200 SSE reply in the stage.
pub(crate) fn stage(
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    Box::pin(async_stream::stream! {
        let mut inner = inner;
        let mut joiner = Joiner::default();
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) => {
                    if let Some(bytes) = joiner.push(&chunk) {
                        yield Ok(bytes);
                    }
                }
                Err(error) => {
                    if let Some(bytes) = joiner.finish() {
                        yield Ok(bytes);
                    }
                    yield Err(error);
                    return;
                }
            }
        }
        if let Some(bytes) = joiner.finish() {
            yield Ok(bytes);
        }
    })
}

#[derive(Default)]
struct Joiner {
    events: SseFramer,
    /// The index the next block sent on gets: blocks are renumbered, since
    /// joined and dropped ones leave gaps.
    next_index: u64,
    /// The index of the block passing through.
    current: u64,
    /// Whether the reply has a `tool_use` block so far. A signature can
    /// arrive after a call the client already has, and a call's signature
    /// must go back to Google, so such a reply keeps its last carriers.
    calls_tool: bool,
    /// Whether a run is open: the current block is a text block the client
    /// has, whose stop is withheld.
    in_run: bool,
    /// The carriers after the run's text, each as its events' payloads.
    carriers: Vec<Vec<Value>>,
    /// A `thinking` block after the run's text, held until its stop shows
    /// whether it is a carrier.
    thinking: Option<Vec<Value>>,
    /// How many carriers the run dropped between parts of its text.
    dropped: usize,
}

impl Joiner {
    /// The events `chunk` completes, as the client gets them.
    fn push(&mut self, chunk: &[u8]) -> Option<Bytes> {
        self.events.push(chunk);
        let mut out = Vec::new();
        while let Some(event) = self.events.next_event() {
            self.event(&event, &mut out);
        }
        (!out.is_empty()).then(|| Bytes::from(out))
    }

    /// What is still held when the stream ends, cut short or not.
    fn finish(mut self) -> Option<Bytes> {
        let mut out = Vec::new();
        self.end_run(true, &mut out);
        out.extend_from_slice(&self.events.take_rest());
        (!out.is_empty()).then(|| Bytes::from(out))
    }

    fn event(&mut self, raw: &[u8], out: &mut Vec<u8>) {
        let Some(data) = payload(raw) else {
            out.extend_from_slice(raw);
            return;
        };
        match data["type"].as_str() {
            Some("content_block_start") => self.block_start(raw, data, out),
            Some("content_block_delta") => self.block_delta(raw, data, out),
            Some("content_block_stop") => self.block_stop(raw, data, out),
            Some("ping") | None => out.extend_from_slice(raw),
            Some(kind) => {
                self.end_run(kind != "message_delta" || self.calls_tool, out);
                out.extend_from_slice(raw);
            }
        }
    }

    fn block_start(&mut self, raw: &[u8], data: Value, out: &mut Vec<u8>) {
        let kind = data["content_block"]["type"].as_str().unwrap_or_default();
        if self.in_run && self.thinking.is_none() {
            match kind {
                "text" => {
                    self.dropped += self.carriers.len();
                    self.carriers.clear();
                    return;
                }
                "thinking" => {
                    self.thinking = Some(vec![data]);
                    return;
                }
                _ => {}
            }
        }
        self.end_run(true, out);
        self.current = self.take_index();
        self.calls_tool |= kind == "tool_use";
        self.in_run = kind == "text";
        self.send(Some(raw), data, out);
    }

    fn block_delta(&mut self, raw: &[u8], data: Value, out: &mut Vec<u8>) {
        match &mut self.thinking {
            Some(block) => block.push(data),
            None => self.send(Some(raw), data, out),
        }
    }

    fn block_stop(&mut self, raw: &[u8], data: Value, out: &mut Vec<u8>) {
        if let Some(mut block) = self.thinking.take() {
            block.push(data);
            if is_carrier(&block) {
                self.carriers.push(block);
            } else {
                // Thinking with text ends the run and goes on after it.
                self.end_run(true, out);
                self.send_block(block, out);
            }
        } else if !self.in_run {
            // A run's text block stays open until the run ends.
            self.send(Some(raw), data, out);
        }
    }

    /// Closes the run's text block, then sends on what the run held or
    /// drops it.
    fn end_run(&mut self, keep_carriers: bool, out: &mut Vec<u8>) {
        if !std::mem::take(&mut self.in_run) {
            return;
        }
        frame(
            &json!({"type": "content_block_stop", "index": self.current}),
            out,
        );
        // A thinking block the stream cut short goes the carriers' way.
        let mut held = std::mem::take(&mut self.carriers);
        held.extend(self.thinking.take());
        let mut dropped = std::mem::take(&mut self.dropped);
        if keep_carriers {
            for block in held {
                self.send_block(block, out);
            }
        } else {
            dropped += held.len();
        }
        if dropped > 0 {
            tracing::debug!(
                carriers = dropped,
                "dropped the signature carriers of a text"
            );
        }
    }

    const fn take_index(&mut self) -> u64 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    /// Sends a held block on as the next one.
    fn send_block(&mut self, block: Vec<Value>, out: &mut Vec<u8>) {
        self.current = self.take_index();
        for event in block {
            self.send(None, event, out);
        }
    }

    /// Sends a block event on under the current index: `raw`, the event as
    /// it came, when that is the index it has, else `data` renumbered.
    fn send(&self, raw: Option<&[u8]>, mut data: Value, out: &mut Vec<u8>) {
        if let Some(raw) = raw
            && data["index"].as_u64() == Some(self.current)
        {
            out.extend_from_slice(raw);
            return;
        }
        data["index"] = Value::from(self.current);
        frame(&data, out);
    }
}

/// Whether a whole `thinking` block is a carrier: one with no thinking in
/// it.
fn is_carrier(block: &[Value]) -> bool {
    block.iter().all(|event| {
        event["delta"]["thinking"]
            .as_str()
            .is_none_or(str::is_empty)
    })
}

/// An event's JSON payload; `None` for anything else, which passes through.
fn payload(event: &[u8]) -> Option<Value> {
    let event = parse_event(std::str::from_utf8(event).ok()?)?;
    serde_json::from_str(&event.data).ok()
}

/// Writes `data` as an event named for its type, as the Messages API names
/// every event.
fn frame(data: &Value, out: &mut Vec<u8>) {
    let name = data["type"].as_str().unwrap_or_default();
    out.extend_from_slice(format!("event: {name}\ndata: {data}\n\n").as_bytes());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A text reply as the child streamed it on 2026-11-18 (`CLIProxyAPI`
    /// 8.0.69, `gemini-4-argon`), with its blank line after every event, the
    /// signature shortened: a block per text part, then the carrier.
    pub(crate) const RECORDED_TEXT_REPLY: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"YVv-aqzDHdbZodAPm4KUkAs\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"gemini-4-argon\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":87,\"output_tokens\":0}}}\n\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Tide pools are\"}}\n\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\" rocky basins along the shore that trap seawater when the ocean recedes at low tide. These temporary\"}}\n\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\" lagoons serve as vibrant mini-ecosystems.\"}}\n\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"cpa-gemini-carrier-v1:previous:text:RW93QkNva0JBV2tVZlJOT2RGTzcw\"}}\n\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":3}\n\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":124,\"output_tokens\":72}}\n\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n\n",
    );

    const RECORDED_TEXT: &str = "Tide pools are rocky basins along the shore that trap seawater \
                                 when the ocean recedes at low tide. These temporary lagoons \
                                 serve as vibrant mini-ecosystems.";

    fn event(data: &Value) -> String {
        let mut out = Vec::new();
        frame(data, &mut out);
        String::from_utf8(out).unwrap()
    }

    fn start(index: u64, block: &Value) -> String {
        event(&json!({"type": "content_block_start", "index": index, "content_block": block}))
    }

    fn delta(index: u64, delta: &Value) -> String {
        event(&json!({"type": "content_block_delta", "index": index, "delta": delta}))
    }

    fn stop(index: u64) -> String {
        event(&json!({"type": "content_block_stop", "index": index}))
    }

    fn text(index: u64, text: &str) -> String {
        [
            start(index, &json!({"type": "text", "text": ""})),
            delta(index, &json!({"type": "text_delta", "text": text})),
            stop(index),
        ]
        .concat()
    }

    fn carrier(index: u64, signature: &str) -> String {
        [
            start(index, &json!({"type": "thinking", "thinking": ""})),
            delta(
                index,
                &json!({"type": "signature_delta", "signature": signature}),
            ),
            stop(index),
        ]
        .concat()
    }

    fn tool_use(index: u64, id: &str) -> String {
        [
            start(
                index,
                &json!({"type": "tool_use", "id": id, "name": "Bash", "input": {}}),
            ),
            delta(
                index,
                &json!({"type": "input_json_delta", "partial_json": "{\"command\":\"ls\"}"}),
            ),
            stop(index),
        ]
        .concat()
    }

    fn end(stop_reason: &str) -> String {
        [
            event(&json!({"type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"input_tokens": 10, "output_tokens": 5}})),
            event(&json!({"type": "message_stop"})),
        ]
        .concat()
    }

    /// The stream the client gets, fed `step` bytes at a time.
    fn joined(stream: &str, step: usize) -> String {
        let mut joiner = Joiner::default();
        let mut out = Vec::new();
        for chunk in stream.as_bytes().chunks(step) {
            out.extend(joiner.push(chunk).into_iter().flatten());
        }
        out.extend(joiner.finish().into_iter().flatten());
        String::from_utf8(out).unwrap()
    }

    /// The content blocks a client assembles from `stream`, in the order
    /// they stop, which is the order Claude Code keeps them in. Panics on a
    /// stream the Messages API would not send: indexes out of order, a
    /// block opened inside another, or one left open at the end.
    pub(crate) fn blocks(stream: &str) -> Vec<Value> {
        let mut blocks = Vec::new();
        let mut open: Option<Value> = None;
        let mut next_index = 0;
        for raw in stream.split("\n\n").filter(|raw| !raw.trim().is_empty()) {
            let data = payload(raw.as_bytes()).unwrap();
            match data["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert!(open.is_none(), "a block starts inside another: {stream}");
                    assert_eq!(data["index"], next_index, "{stream}");
                    open = Some(data["content_block"].clone());
                }
                "content_block_delta" => {
                    assert_eq!(data["index"], next_index, "{stream}");
                    let block = open.as_mut().expect("a delta outside a block");
                    for (field, part) in [
                        ("text", "text"),
                        ("thinking", "thinking"),
                        ("signature", "signature"),
                        ("partial_json", "partial_json"),
                    ] {
                        if let Some(more) = data["delta"][field].as_str() {
                            let so_far = block[part].as_str().unwrap_or_default();
                            block[part] = Value::from(format!("{so_far}{more}"));
                        }
                    }
                }
                "content_block_stop" => {
                    assert_eq!(data["index"], next_index, "{stream}");
                    blocks.push(open.take().expect("a stop outside a block"));
                    next_index += 1;
                }
                "message_delta" | "message_stop" => {
                    assert!(open.is_none(), "the reply ends inside a block: {stream}");
                }
                _ => {}
            }
        }
        assert!(open.is_none(), "the stream ends inside a block: {stream}");
        blocks
    }

    fn kinds(stream: &str) -> Vec<String> {
        blocks(stream)
            .iter()
            .map(|block| block["type"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_recorded_text_reply_becomes_one_text_block_and_ends_on_it() {
        for step in [1, 7, 256, usize::MAX] {
            let out = joined(RECORDED_TEXT_REPLY, step);
            assert_eq!(
                blocks(&out),
                [json!({"type": "text", "text": RECORDED_TEXT})],
                "{step}"
            );
            // The text still streams part by part, and the rest of the
            // reply is as it came.
            assert_eq!(out.matches("text_delta").count(), 3);
            let tail = RECORDED_TEXT_REPLY.find("event: message_delta").unwrap();
            assert!(out.ends_with(&RECORDED_TEXT_REPLY[tail..]), "{out}");
            assert!(out.starts_with("event: message_start"), "{out}");
        }
    }

    #[test]
    fn a_tool_call_reply_passes_through_byte_for_byte() {
        // The shape the child gives a Gemini tool call: its signature's
        // carrier, the call, and an empty text block.
        let reply = [
            carrier(0, "EowBCokBAWkUfRNBgAbI27REJtyy"),
            tool_use(1, "Bash-1"),
            tool_use(2, "Bash-2"),
            start(3, &json!({"type": "text", "text": ""})),
            delta(3, &json!({"type": "text_delta", "text": ""})),
            stop(3),
            end("tool_use"),
        ]
        .concat();
        assert_eq!(joined(&reply, 11), reply);
    }

    #[test]
    fn carriers_between_text_and_a_tool_call_keep_their_place() {
        let reply = [
            text(0, "I'll list"),
            text(1, " the files."),
            carrier(2, "cpa-gemini-carrier-v1:previous:text:dGV4dA"),
            carrier(3, "EowBCokBAWkUfRNBgAbI27REJtyy"),
            tool_use(4, "Bash-1"),
            end("tool_use"),
        ]
        .concat();
        let out = joined(&reply, 64);
        assert_eq!(
            blocks(&out),
            [
                json!({"type": "text", "text": "I'll list the files."}),
                json!({"type": "thinking", "thinking": "",
                       "signature": "cpa-gemini-carrier-v1:previous:text:dGV4dA"}),
                json!({"type": "thinking", "thinking": "",
                       "signature": "EowBCokBAWkUfRNBgAbI27REJtyy"}),
                json!({"type": "tool_use", "id": "Bash-1", "name": "Bash", "input": {},
                       "partial_json": "{\"command\":\"ls\"}"}),
            ]
        );
    }

    #[test]
    fn text_after_a_tool_call_keeps_its_trailing_carrier() {
        let reply = [
            carrier(0, "EowBCokBAWkUfRNBgAbI27REJtyy"),
            tool_use(1, "Bash-1"),
            text(2, "Running"),
            text(3, " it."),
            carrier(4, "cpa-gemini-carrier-v1:previous:text:dGV4dA"),
            end("tool_use"),
        ]
        .concat();
        assert_eq!(
            kinds(&joined(&reply, 64)),
            ["thinking", "tool_use", "text", "thinking"]
        );
    }

    #[test]
    fn a_carrier_between_two_parts_of_a_text_is_dropped() {
        let reply = [
            carrier(0, "bGVhZGluZw"),
            text(1, "One"),
            carrier(2, "cpa-gemini-carrier-v1:previous:text:b25l"),
            text(3, " two"),
            start(4, &json!({"type": "text", "text": ""})),
            delta(4, &json!({"type": "text_delta", "text": ""})),
            stop(4),
            end("end_turn"),
        ]
        .concat();
        assert_eq!(
            blocks(&joined(&reply, 64)),
            [
                // A carrier before the text is not in its way.
                json!({"type": "thinking", "thinking": "", "signature": "bGVhZGluZw"}),
                json!({"type": "text", "text": "One two"}),
            ]
        );
    }

    #[test]
    fn thinking_with_text_ends_the_run_and_passes_through() {
        let reply = [
            text(0, "One"),
            start(1, &json!({"type": "thinking", "thinking": ""})),
            delta(1, &json!({"type": "thinking_delta", "thinking": ""})),
            delta(1, &json!({"type": "thinking_delta", "thinking": "hm"})),
            delta(1, &json!({"type": "signature_delta", "signature": "c2ln"})),
            stop(1),
            text(2, "two"),
            end("end_turn"),
        ]
        .concat();
        assert_eq!(
            blocks(&joined(&reply, 64)),
            [
                json!({"type": "text", "text": "One"}),
                json!({"type": "thinking", "thinking": "hm", "signature": "c2ln"}),
                json!({"type": "text", "text": "two"}),
            ]
        );
    }

    #[test]
    fn a_cut_stream_and_an_error_close_the_text_and_lose_nothing() {
        let cut = [
            text(0, "One"),
            carrier(1, "cpa-gemini-carrier-v1:previous:text:b25l"),
            "event: content_block_start\ndata: {\"type\"".to_string(),
        ]
        .concat();
        let out = joined(&cut, 64);
        let (whole, partial) =
            out.split_at(out.len() - "event: content_block_start\ndata: {\"type\"".len());
        assert_eq!(kinds(whole), ["text", "thinking"]);
        assert_eq!(partial, "event: content_block_start\ndata: {\"type\"");

        let error = event(&json!({"type": "error",
            "error": {"type": "api_error", "message": "upstream closed"}}));
        let failed = [text(0, "One"), text(1, " two"), error.clone()].concat();
        let out = joined(&failed, 64);
        assert_eq!(blocks(&out), [json!({"type": "text", "text": "One two"})]);
        assert!(out.ends_with(&error), "{out}");
    }

    #[test]
    fn pings_and_unreadable_events_pass_through_without_ending_the_run() {
        let ping = "event: ping\ndata: {\"type\":\"ping\"}\n\n";
        let reply = [
            text(0, "One"),
            ping.to_string(),
            ": keep-alive\n\n".to_string(),
            text(1, " two"),
            end("end_turn"),
        ]
        .concat();
        let out = joined(&reply, 64);
        assert_eq!(
            blocks(&out.replace(": keep-alive\n\n", "")),
            [json!({"type": "text", "text": "One two"})]
        );
        assert!(
            out.contains(ping) && out.contains(": keep-alive\n\n"),
            "{out}"
        );
    }
}
