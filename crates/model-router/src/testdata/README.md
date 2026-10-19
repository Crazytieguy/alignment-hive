# Test data

- `xai-websearch-basic.sse` — **byte-faithful** excerpt of a real
  `POST /v1/responses` stream from a managed `CLIProxyAPI` child answering a
  `grok-4.5` hosted `web_search` (captured 2026-08-04 with `curl --no-buffer`,
  event framing untouched, trailing `response.completed` dropped for size).
  Contains response bodies only — no credentials.
- `xai-websearch-dataonly.sse` — **synthetic**, hand-written. Covers an SSE
  shape the child has *not* been observed to emit (events with no `event:`
  line, and one event split across two `data:` lines). It pins the parser's
  defensive behavior; it is not evidence of upstream behavior.
- `meta-websearch-basic.sse` — **byte-faithful**, complete (nothing dropped)
  `POST /v1/responses` stream from a managed `CLIProxyAPI` 8.0.30 child
  answering a `muse-wonder-1.0` hosted `web_search` with `tool_choice: auto`,
  `max_tool_calls: 1`, `include: ["web_search_call.action.sources"]` and low
  reasoning effort (captured 2026-10-15 by reading the stream line by line
  and writing the lines back unchanged). One `search` action with 12
  title-less URL sources, then the answer text. Contains response bodies only
  (encrypted reasoning, no credentials).
