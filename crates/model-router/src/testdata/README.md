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
