# model-router: measured dependency behavior

Each section records its own date, the version of the dependency it was
measured against, and the method.

## Agent tool, Workflow, and effort (Claude Code 2.1.216)

Method: real Claude Code driver sessions (sonnet main) pointed at the router
(`ANTHROPIC_BASE_URL=http://127.0.0.1:8787`) with capture mode on. "Emitted"
below = observed in captured request bodies/headers.

### Agent tool `model` parameter
- The Agent tool's `model` parameter is a closed, harness-enforced enum
  (`sonnet | opus | haiku | fable`). Passing a GPT routing ID fails with
  `InputValidationError` before dispatch.
- `ANTHROPIC_CUSTOM_MODEL_OPTION` does not extend the enum.
- Neither does the 2.1.243 `modelPicker` setting (re-verified on 2.1.246 —
  see the `modelPicker` section).
- Gateway model discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`)
  never fired in `-p` sessions (zero `GET /v1/models` hits, no
  `~/.claude/cache/gateway-models.json`).
- The Agent tool has no `effort` parameter.
- Binary inspection (2.1.216 bundle): the enum is a hard-coded Zod literal at
  the Agent tool schema site with no env/flag/gateway conditional anywhere
  near it; the error is Zod's generic invalid_value formatter. Docs are
  silent on the per-invocation parameter's accepted values (the documented
  "full model ID" allowance is the *frontmatter/SDK* field, which works).

### Workflow tool — per-invocation model AND effort work (measured live)
Workflow scripts' `agent()` types `opts.model` as a plain string and skips
the Agent tool's enum validation entirely. Measured on 2.1.216:
- `agent(prompt, {model: 'claude-gpt-5.6-sol'})` ran the worker on the GPT
  routing ID (confirmed in capture) — no error.
- `agent(prompt, {model: 'claude-gpt-5.6-sol', effort: 'low'})` emitted
  `output_config.effort: low` on the worker's requests.
- `agent(prompt, {agentType: 'gpt'})` resolves a project agent definition and
  inherits its GPT model.
The Workflow `opts.model` string-typing is an implementation detail, not a
documented contract — retest on Claude Code upgrades.

### Dynamic effort
- `output_config.effort` IS emitted for unrecognized (GPT) model IDs, without
  `CLAUDE_CODE_ALWAYS_ENABLE_EFFORT` (docs imply otherwise — measured on
  2.1.216: session default "high" was sent).
- Agent frontmatter `effort: low` → worker request emitted `effort: low`.
- `CLAUDE_CODE_EFFORT_LEVEL=medium` → emitted `effort: medium`.
- CLIProxyAPI (v7.2.92, code-verified) maps `output_config.effort` →
  Codex `reasoning.effort`; gpt-5.6-sol/terra support low..xhigh+max+ultra.

## Other measured facts (Claude Code 2.1.216)
- `thinking: {type: adaptive}` is sent unconditionally to unrecognized model
  IDs (matches docs). With effort also emitted, CLIProxyAPI maps
  adaptive+effort → same-named `reasoning.effort`; adaptive WITHOUT effort
  would default to xhigh.
- OAuth capability beta string: `oauth-2025-04-20`, in an
  `anthropic-beta` list alongside `claude-code-20250219`,
  `interleaved-thinking-2025-05-14`, `effort-2025-11-24`,
  `context-management-2025-06-27`, etc. (E1: subscription OAuth session
  through router returned 200 and billed to the subscription login.)
- Subagent system prompts are the agent-definition body only (plus a small
  billing-header block: `cc_is_subagent=true`).

## CLIProxyAPI facts (codex research, v7.2.92, code-verified)
- Needs its own Codex OAuth (`cliproxyapi -codex-login`, browser +
  loopback:1455); no supported import of `~/.codex/auth.json`.
- Translation is selective: tools/tool_choice/images/thinking translated;
  `cache_control`, `stop_sequences`, `metadata`, `max_tokens`, `defer_loading`
  stripped/dropped; SSE synthesized. Valid Codex slugs: `gpt-5.6-sol`,
  `gpt-5.6-terra`, `gpt-5.6-luna` (bare `gpt-5.6` is not a slug).

## Usage + caching SSE measurements (2026-07-20, follow-up session)

Method: two identical streamed `/v1/messages` requests (long system block with
`cache_control`) sent through the running tokened router to `claude-gpt-5.6-sol`;
raw SSE inspected.

- Prompt caching on the GPT branch WORKS end to end: run 1 reported
  `input_tokens: 2411`; run 2 `input_tokens: 619,
  cache_read_input_tokens: 1792`. CLIProxyAPI strips `cache_control` but
  Codex-side automatic prefix caching kicks in anyway, and cached-token
  counts come back translated into the Anthropic usage field. No
  `cache_creation_input_tokens` is ever reported (OpenAI doesn't expose it).
- Live token ticking is broken by SSE shape, not by buffering:
  `message_start` carries `usage: {input_tokens: 0, output_tokens: 0}` and
  real usage (including `input_tokens` + cache fields — non-standard
  placement) arrives only in the final `message_delta`. Claude Code seeds the
  live subagent token display from `message_start` usage, so GPT subagent
  counts sit at 0 mid-run and completion notifications report
  `subagent_tokens: 0`. Root cause is structural: OpenAI streaming only
  reports usage in the final chunk, so a translator cannot know exact input
  tokens at `message_start` time.
- Context length (binary-verified, 2.1.216): the default context window for
  unknown model IDs is exactly 200000 (constant `EYt` in the window-sizing
  function). Undocumented override: `CLAUDE_CODE_MAX_CONTEXT_TOKENS` applies
  to any model whose normalized ID does NOT start with `claude-` (Claude
  models keep their built-in windows). Gateway model discovery silently
  drops IDs that don't start with `claude`/`anthropic`.
- Claude context windows behind the gateway (measured 2026-07-28, Claude Code
  2.1.220). Claude Code grants a natively-1M Claude model its 1M window only
  when `new URL(ANTHROPIC_BASE_URL).host === "api.anthropic.com"`; behind the
  router that check fails, so `claude-fable-5`, `claude-opus-5` and
  `claude-sonnet-5` report 200000. Measured with
  `claude -p --output-format json | jq '.modelUsage[].contextWindow'`, which
  returns the same number that drives auto-compaction, the model's own
  remaining-budget reminders, and file-read caps (`max(40000, window × 0.05 ×
  3)`). Not cosmetic: a 200K window compacts a Fable session five times too
  early. Three routes to 1M, all measured: (a) a `[1m]` suffix on the model
  string — `fable[1m]`, `claude-fable-5[1m]` and the alias forms all give
  1000000, and the suffix never reaches the wire (the router logs
  `model="claude-fable-5"`); (b) `ANTHROPIC_DEFAULT_{OPUS,SONNET,FABLE}_MODEL`
  set to a `[1m]`-suffixed concrete ID — works but pins the model version, and
  the alias form (`fable[1m]`) breaks, sending a literal `model: "fable"`
  upstream; (c) `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL=1`, which satisfies
  the base-URL check itself and fixes bare IDs everywhere, including agent
  definitions the user does not control.
  `ANTHROPIC_BETAS=context-1m-2025-08-07` is a no-op: it reaches the wire but
  never the window calculation. The `/model` picker is unaffected either way —
  the server ships its Fable entry as `claude-fable-5[1m]` already
  (`additionalModelOptionsCache` in `~/.claude.json`), and subagents that omit
  `model` inherit the parent's suffixed string. The gap the flag closes is
  subagents with an explicit model: an agent defined `model: fable` measured
  200000 while its parent ran at 1000000 in the same session.
- `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` on GPT IDs (verified
  2026-07-28). The 1M window is registry-gated and GPT IDs have no
  registry entry (measured: `gpt-5.6-sol` still reports 250000 with the flag
  on); refusal fallback is gated on the `refusal_fallback` model capability,
  which only `claude-opus-5`/`claude-fable-5` carry, and is not a wire
  parameter; the extra `auto-mode-classifier` beta applies only to auto-mode
  classifier queries, which run on a Claude model; the prompt-cache-scope beta
  was already being sent (`DH()` is true regardless of the flag) and the
  `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` sentinel does not leak into the GPT
  system prompt (asked the model directly). Non-beta
  headers (`traceparent`, first-party billing `cch=00000;`) do reach the GPT
  upstream, and two behaviours behind their own experiment flags
  (fine-grained tool streaming `tengu_fgts`, image limits
  `tengu_crimson_vector`, both default off) would apply session-wide if
  Anthropic enables them. End-to-end check: a GPT-5.6 Sol request with tool
  use succeeded with the flag on.
- Real GPT-5.6 context via Codex (researched 2026-07-21): the OpenAI API
  advertises 1.05M for sol/terra (128K max output), but the Codex/ChatGPT
  backend — our upstream — serves a reduced catalog: 272K input + 128K output
  with a 95% multiplier, ~258.4K effective input (openai/codex#32806,
  InfoWorld 2026-07).

## Auto-compact accounting (binary-verified, 2026-07-27, Claude Code 2.1.220)

Method: symbol search and disassembly-adjacent string extraction from the
installed 2.1.220 binary.

- The auto-compact gate computes its token total as
  `YA(messages, model) = dIe(usage of the most recent message carrying usage)
  + tP(messages after that anchor)`. The first term is **API-reported usage**;
  only the trailing messages are estimated client-side.
- `dIe(u) = u.input_tokens + (u.cache_creation_input_tokens ?? 0) +
  (u.cache_read_input_tokens ?? 0) + u.output_tokens` — exactly four fields.
- The total is compared against
  `CSe(model, window) = aY(model, window).window - min(outputReserve, 20000)`.
- `countTokensWithFallback` (API `count_tokens` with a haiku fallback) backs
  the `/context` display and the system-prompt / CLAUDE.md size analysis, and
  is **not** on the compact path. So the router's `count_tokens` 404 does not
  affect compaction.
- The unscaled post-anchor tail is in real tokens while the anchor is scaled,
  so a scaled-down route over-counts its tail slightly and compacts marginally
  early — the safe direction.
- Prefix rule re-verified on this version:
  `if (n !== undefined && n > 0 && !normalized.startsWith("claude-")) return n;
  return <200000 default>`.
- Also observed: `CLAUDE_CODE_AUTO_COMPACT_WINDOW` (clamped to [1e5, 1e6]) and
  an `autoCompactWindow` setting can *lower* the compaction window but never
  raise it above the model's max; both are global, so neither gives per-model
  sizing.
- This is undocumented, reverse-engineered behavior — a Claude Code version
  that starts sizing context differently would turn scaling into silent
  overruns. *(Re-verified against 2.1.223 — see the context-window changes
  section below.)*

Externally sourced catalog facts (not measured here, 2026-07-27): GLM-5.2
advertises a 1M-token window under the separate `glm-5.2[1m]` model ID
(base ID serves less); Kimi K3 is listed at 1,048,576 on OpenRouter. Host-
served windows can be lower than the vendor's advertised number — the Codex
backend serving 272K of GPT-5.6's advertised 1.05M is the same pattern — and
OpenRouter's `context_length` is the maximum across its sub-providers.

## Context-window changes in Claude Code 2.1.223 (binary-verified, 2026-08-06)

Method: string extraction from the installed 2.1.223 binary, as for 2.1.220
above (whose retest trigger this satisfies). Changelog
context: 2.1.223 changes `CLAUDE_CODE_DISABLE_1M_CONTEXT` behavior, enforces
assumed context limits on unrecognized model IDs, and adds a startup warning
around both.

Core assumptions re-verified byte-for-byte equivalent:

- Prefix rule unchanged: `CLAUDE_CODE_MAX_CONTEXT_TOKENS` applies exactly when
  the resolved model ID does not start with `claude-`; otherwise the 200K
  unknown-model default. Routed IDs still get the declared 258400.
- Gate arithmetic unchanged: anchor = most recent usage-bearing assistant
  message; sum of exactly `input_tokens + cache_creation_input_tokens +
  cache_read_input_tokens + output_tokens` (`yHe`); client-estimated unscaled
  tail after the anchor; 20K output reserve; 13K compact margin.

New in 2.1.223:

- Window resolution (`v9`) now tags a source. Unrecognized model IDs — every
  routed ID — resolve as source `"unknown-model"` (window still the
  `CLAUDE_CODE_MAX_CONTEXT_TOKENS` value); previously they fell through to
  `"auto"`. The auto-compact gate (`hky`) short-circuits to *disabled* when
  the source is `"auto"`, so this change is what makes the tokens gate firmly
  cover routed models.
- `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1`
  ("restores the previous wait-for-the-API behavior") reverts routed models
  to source `"auto"` — the tokens gate never fires.
- The new unrecognized-model startup notice ("X is not a model this version
  of Claude Code recognizes…") is suppressed exactly when the model is
  non-`claude-` and `CLAUDE_CODE_MAX_CONTEXT_TOKENS` > 0. Without the
  declaration the notice appears and the assumed window is 200K.
- `CLAUDE_CODE_DISABLE_1M_CONTEXT` now clamps native-1M Claude models to 200K
  via auto-compact, and emits a startup warning for any model whose window
  exceeds 200K with source ≠ `"auto"` — which includes every routed model
  (258400 > 200K). A user setting that var sees "the 200K limit isn't
  enforced for gpt-5.6-sol…" suggesting `CLAUDE_CODE_AUTO_COMPACT_WINDOW=
  200000`.
- `CLAUDE_CODE_AUTO_COMPACT_WINDOW` / `autoCompactWindow` semantics unchanged
  (lower-only, clamped to [100K, 1M], global). On a scaled route the
  real-token trigger shrinks proportionally: `configured × actual/declared`.
- Account state `longContext1mCreditsBlocked` (set when the API refuses 1M
  for credit reasons) clamps *any* model with a resolved window above 200K to
  200K — routed models included.
- `modelOverrides` (the notice's suggested remedy) is a model *aliasing* map,
  not per-model windows. Alias resolution runs before
  the `claude-` prefix check, so mapping a routed ID to a `claude-*` name
  would strip the env-var window.

## Harness facts from the GPT tool-usage audits (2026-07-20/21, Claude Code 2.1.217)

- Subagent Writes of report/findings `.md` files
  are hard-blocked by Claude Code ("return findings as text").
- The bundled claude-api skill's
  imperative trigger fires on any "Claude Code" mention in a prompt, costing
  a turn each time (once 784KB of context).

### Grep/Glob and shell search (2026-07-28)

Claude Code 2.1.117 removed the standalone Grep/Glob tools on native
macOS/Linux builds — search moved to embedded `bfs`/`ugrep` via Bash
(2.1.162 restores the tools only when `--tools` names them explicitly).
Last transcript on this machine with a real Grep/Glob call: 2026-04-27,
v2.1.119.

Verified in a live Bash session (2.1.220): plain `find`/`grep` are
shadowed by shell-snapshot functions re-execing the `claude` multicall
binary as bfs/ugrep, the `grep` shim defaulting to `--ignore-files
--hidden -I --exclude-dir=.git` (+ .svn/.hg/.bzr/.jj/.sl). `rg`
resolves to the user's real ripgrep when installed, so `--hidden`
sweeping `.git/` remains possible there.

## Effort and windows on openai-compatibility upstreams (2026-07-28, CLIProxyAPI 7.2.92)

Method: ran the cached CLIProxyAPI binary against a local fake
OpenAI-compatible host that logs the exact body it receives, with one
`openai-compatibility` provider configured. No provider account involved.

- `output_config.effort` **is** forwarded, as OpenAI's top-level
  `reasoning_effort`, verbatim: low/medium/high/xhigh/max each arrived
  unchanged.
- With `thinking: {type: adaptive}` and **no** effort, the forwarded body
  carries `reasoning_effort: "xhigh"` — the same adaptive-defaults-to-xhigh
  behavior noted for the Codex path, and it reaches openai-compat hosts too.
  With neither field, no `reasoning_effort` is sent.
- Live follow-up against OpenRouter + Kimi K3 (real key, ~$0.05): all five
  Claude Code levels (low/medium/high/xhigh/max) return 200. Values outside
  Kimi's documented low/high/max are NOT rejected, so the feared 400 does not
  happen. Whether a level changes behavior is a different question and the
  answer is "sometimes": pinned to Together on one prompt, low/medium/high
  were identical (68 reasoning tokens), xhigh 74, max 64-but-139-completion;
  on an earlier prompt max produced 92 reasoning tokens against 13 for low.
  So effort is forwarded and accepted, and its effect is host- and
  model-dependent — worth measuring per host before promising anything.
- Incidental confirmation of the routing lottery: five identical one-token
  requests were served by DigitalOcean, Together (x3) and Fireworks.
- Also visible in the forwarded body: `stream_options: {include_usage: true}`,
  so real usage does come back on the openai-compat path, not just the Codex
  one.

### OpenRouter sub-provider windows vary enormously (2026-07-28)

`GET /api/v1/models/moonshotai/kimi-k3/endpoints` (public, no key) lists seven
endpoints: BaseTen, DigitalOcean, Fireworks x2, Together, Moonshot AI all at
1,000,000–1,048,576 — and **Nebius at 8,000**. The aggregate `/models` entry
reports 1,048,576. OpenRouter's provider-routing docs describe filtering on
`max_tokens` and on supported parameters, but say nothing about routing by
prompt size, so a long conversation can be handed to the 8K endpoint. Every
endpoint lists `reasoning_effort` in `supported_parameters`. There is no
model-slug pin; `:nitro` and `:floor` only change sorting.

## Per-session WebSearch budget (2026-07-29, Claude Code 2.1.220)

Observed during the web-research bake-off (not documented anywhere we
found): a Claude Code session has a ~200-call WebSearch budget shared
across the main loop, subagents, and Workflow agents. Once exhausted,
WebSearch calls return a budget-exhausted notice ("200 of 200 WebSearch
calls") and agents silently degrade to WebFetch-only — this invalidated a
36-agent judging round mid-run before it was noticed; the only in-band
signal is the notice in each affected agent's transcript. Fresh `claude
-p` sessions each get their own budget. Undocumented harness
behavior — recheck the cap and its scope on Claude Code upgrades.

## WebSearch on GPT main-loop sessions (2026-07-21, Claude Code 2.1.217)

- Mechanism (2.1.217 bundle): the WebSearch tool issues a side `/v1/messages`
  call on the session's **main-loop model** (on 2.1.220: a Claude
  small-fast model — see the phase-5 measurement) whose only tool is the
  server-side `web_search_20250305` tool, then parses links out of
  `web_search_tool_result` blocks; a result block with empty `content`
  renders as "No links found." (A statsig gate, `tengu_plum_vx3`, would
  switch the side call to the small-fast model; observed off.) The bundle
  source sets `tool_choice: {type: tool, name: web_search}`, but the live
  captured request carries `tool_choice: auto`.
- Through CLIProxyAPI 7.2.92, Codex runs the search but returns links only as
  inline text citations; CLIProxyAPI (PR #3868) maps the `web_search_call`
  into the Anthropic block pair with `content: []`. Upstream declined to go
  further (issue #3132, closed NOT_PLANNED) — the Codex Responses endpoint
  exposes no structured sources on that path (`annotations` empty,
  `include: ["web_search_call.action.sources"]` not honored).
- The Codex CLI itself (gpt-5.6) doesn't use that path: its `web.run` tool
  POSTs to `{provider}/alpha/search` and gets structured `results` back.
- Benchmark (3 queries, via the gateway): legacy LLM side call 20.8–71.7s
  with 2–4 scraped-able inline links; `alpha/search` 0.9–2.6s with 32–35
  structured results. One UA quirk: the backend 403s `Python-urllib/*`
  user agents (curl and no-UA pass).
- Live e2e (2026-07-21): `claude -p --model gpt-5.6-sol` WebSearch through
  the intercepting router returned a fully populated Links array in
  ~2s; identical query on the passthrough router returned "No links found."
  after 20–70s. `/v1/alpha/search` is an undocumented endpoint — retest on
  Codex/CLIProxyAPI upgrades.

## WebSearch sub-call attribution (2026-07-22, follow-up on the above)

- The sub-call always runs on the session's **main-loop model** even when a
  subagent invoked WebSearch (verified by capture: gpt-5.6-sol subagent under
  a haiku main produced a Claude-branch sub-call on haiku; on 2.1.220: a
  Claude small-fast model regardless of the main). The sub-call
  carries `cc_is_subagent=true` in its billing-header block but nothing
  identifying the requesting agent's model.

## Context overflow (2026-07-29, Claude Code 2.1.220 + codex-rs HEAD 6493417150 + CLIProxyAPI 7.2.92)

Observed live: a workflow-heavy session on `gpt-5.6-sol` hit
the Codex backend's input limit and retried the identical oversized request
14 times over ~14 minutes with no recovery.

### Claude Code's overflow recovery (binary-verified, 2.1.220)

- Claude Code has an error-driven recovery layer — "reactive compact" — on
  top of the preventive auto-compact gate: classify the failure as
  `prompt_too_long`, summarize the oldest message groups (gap-guided by the
  parsed `N tokens > M` numbers; an unparseable gap degrades to step-1
  progressive compaction), then re-enter the query loop
  (`reactive_compact_retry`) and retry the round trip. One reactive attempt
  per request; the flag resets each round trip.
- Classification is string-matched and reaches `prompt_too_long` only via:
  HTTP 400 with message containing `prompt is too long` / `input is too long
  for requested model` / `` input length and `max_tokens` exceed context
  limit `` (Anthropic/Bedrock phrasings), or HTTP 413 with message
  containing `context window`.
- Eligibility (`QRs`): auto-compact enabled (`DISABLE_COMPACT` /
  `DISABLE_AUTO_COMPACT` / `autoCompactEnabled` off disable it), not the
  compaction request itself (that path has its own trim-oldest retry loop,
  `tengu_compact_ptl_retry`), not summary side-calls. The session-type gate
  (`wSe`) is TRUE for all local sessions — interactive, `-p`, and subagents;
  only `CLAUDE_CODE_REMOTE` cloud sessions sit behind a statsig gate.
  "Compaction impossible" = fewer than 2 message groups (an oversized first
  request), which surfaces an explanatory error instead.
- The preventive gate runs per round trip, not per user turn: the agentic
  driver re-enters itself after each tool batch
  (`transition:{reason:"next_turn"}`) and evaluates the gate
  (`query_autocompact_start`) at the top of every iteration. Exposure per
  check is one round trip's growth — which parallel tool results routinely
  push past any fixed margin, so the error backstop is load-bearing, not a
  corner case.

### Codex's own handling (repo-verified at HEAD 6493417150)

- Windows are backend-advertised via `GET /models`: `context_window: 272000`
  for GPT-5.x plus `effective_context_window_percent: 95` (default) → hard
  cap 258400, and `auto_compact_token_limit` clamped to 90% of raw → soft
  compact trigger ≈244800, checked pre-turn and after every sampling
  response. Codex runs with ~13.6K preventive slack and leans on its
  backstop.
- Overflow detection is exclusively `error.code == "context_length_exceeded"`
  inside a `response.failed` SSE event on a 200 stream — never HTTP status,
  never message text (their tests include a message with an embedded newline
  to keep it that way). Recovery: pin accounted usage to the full window
  (`set_total_tokens_full`) so the next turn force-compacts; no in-turn
  retry. Inside compaction only: a drop-oldest-item retry loop.

### The wire shapes at our boundary (captured live, 2026-07-29)

CLIProxyAPI drops `context_length_exceeded` in translation. What the router
receives from a `gpt-5.6-sol` overflow:

- Non-streaming: HTTP 400,
  `{"type":"error","error":{"type":"invalid_request_error","message":"Your
  input exceeds the context window of this model. Please adjust your input
  and try again."}}`
- Streaming: HTTP 200 `text/event-stream` — `message_start` (with the
  router-estimated `input_tokens`), then `event: error` carrying the same
  JSON error object.

Neither shape matches any of Claude Code's `prompt_too_long` patterns (right
phrase for the 413 rule but wrong status; wrong phrase for the 400 rules),
so no recovery ran — the observed retry loop.

**Retest triggers:** any
CLIProxyAPI upgrade (its error translation may change shape — if it starts
preserving `context_length_exceeded`, detection can tighten to the code),
and Codex backend window changes (272K/95% → update `GPT_CONTEXT_WINDOW`).
(The classifier strings and recovery behavior are reverse-engineered from
Claude Code, so an upgrade could still change them silently.)

### Full recovery e2e: Claude Code compacting on the GPT branch (2026-07-29 evening)

Verified end to end with real `claude -p` sessions on `gpt-5.6-sol` through
the patched router: a session with ~221K of multi-turn history took an
under-estimated ~160K tool result (token-dense CJK: o200k ≈ 1.9 tokens/char
vs the client's ~chars/4 estimate, so the preventive gate passes), the
request overflowed the backend, the router translated the error, and Claude
Code ran the whole recovery invisibly — reactive-compact summarize 9ms after
the error, `compact_boundary` event, retry succeeded, task completed. Total
recovery ~9s; nothing surfaced to the user. Engineering the failure took
several attempts, which mapped the protective layers:

- **Micro-compaction** evicts old *tool results* well before the gate
  (observed: anchors pinned at ~76K across five 21K reads), so tool-result
  bulk alone cannot build overflow pressure; user-message content is not
  evicted.
- The **per-round-trip gate** catches any jump its estimate can see; only
  under-estimated content (non-text blocks; CJK-dense text) gets past it.
- Reactive compact **bails without an assistant message in the compactable
  prefix** ("no assistant messages in summarize set") — a single-exchange
  session with one giant user message surfaces the error instead.
- Bulk in the **current turn** is kept verbatim through compaction (you
  cannot summarize the turn being answered), so a current-turn payload that
  alone exceeds the window is unrecoverable by design — true for Anthropic
  models too.

Separate finding, same evening: the backend's enforcement boundary **moved**.
Morning sessions and recon 400'd at ~260K, but by evening uncached probes
passed at 300K/340K and failed at 380K+ — enforcement now sits somewhere in
(340K, 380K), well above the advertised 272K×95% = 258.4K. Looks like a
rollout in progress (openai/codex#32806 anticipated a restore). Retest
when the backend's `GET /models` catalog (via codex-rs) stabilizes on a new
window.

## Sol Originator gating (2026-07-30, CLIProxyAPI 7.2.92)

`gpt-5.6-sol` through the router failed near-100% while luna/terra worked
and the real Codex CLI (0.146.0) reached sol fine. Surface symptoms were
misleading twice over: agents saw `503 auth_unavailable` (CLIProxyAPI
quarantines the OAuth entry after each upstream failure, so most requests
fast-fail on quarantine), and the underlying failure was an HTTP **200**
whose SSE stream immediately emits `event: error` with
`code: "server_is_overloaded"` + `response.failed` — visible only with
`request-log: true` in the CLIProxyAPI config (records bearer tokens;
enable briefly, delete the logs after).

A/B probes isolated the trigger to the `Originator` header. CLIProxyAPI
hardcodes `Originator: codex-tui` (`codex_executor_request.go`) unless the
inbound request supplies one; the Codex backend load-sheds sol requests
with that fingerprint (4/4 success with `codex_cli_rs`, immediate
"overloaded" with defaults, UA irrelevant — spoofing the CLI User-Agent
alone still failed, `Originator: codex_cli_rs` alone succeeded even with
`curl/8.7.1`). Ruled out along the way: auth/token health (occasional 200s,
CLI works), the injected `image_generation` tool
(`disable-image-generation: "passthrough"` changed nothing), stray
temp-dir cliproxy instances refreshing the same account (killing them
changed nothing), and OpenAI-wide incidents (status pages green).

`CLIProxyAPI` 7.2.110's codex identity cloaking replaces the override
(router 0.1.9 no longer sets the header). Watch for
the backend tightening the fingerprint check (e.g. requiring a matching
`codex_cli_rs/<version>` User-Agent or minimum client version — sol's
catalog entry declares `minimal_client_version: 0.144.0`); if sol-only
"overloaded" errors return, re-run the A/B probes with a current CLI
fingerprint.

## Grok (xAI) phase-4 wire measurements (2026-07-31, CLIProxyAPI 7.2.110, model-router 0.1.9+grok)

Method: a throwaway second router instance (isolated XDG dirs, port 8899) in
external mode against a private CLIProxyAPI child (port 8399) started with
`request-log: true`, its own auth dir holding *copies* of the live
`codex-*.json` / `xai-*.json`. The live gateway (8787/8317) was untouched and
re-verified healthy afterwards. Evidence:
`~/.claude/jobs/13cfa33f/tmp/phase4/evidence/`.

### Reasoning effort does NOT survive on the xAI path

`output_config.effort` — the field an agent file's `effort:` frontmatter
produces — is forwarded verbatim on the Codex and openai-compat paths, but
is **dropped for xAI**. 9/9 requests (low/medium/high x3) against
`grok-4.5` arrived upstream as `reasoning.effort: "medium"`, the default:

```
inbound  {"model":"grok-4.5","output_config":{"effort":"low"},...}
upstream {"model":"grok-4.5","reasoning":{"effort":"medium","summary":"auto"},...}
         -> https://cli-chat-proxy.grok.com/v1/responses
```

Other channels tried, all ineffective: Anthropic `thinking.budget_tokens`,
top-level `reasoning_effort`. The parenthesised model-id suffix
(`internal/thinking/suffix.go`) is the only one that works:

| request model | upstream model | upstream reasoning.effort |
|---|---|---|
| `grok-4.5(low)` | `grok-4.5` | `low` |
| `grok-4.5(high)` | `grok-4.5` | `high` |
| `grok-4.5(xhigh)` | `grok-4.5` | `high` (clamped) |
| `grok-4.5(max)` | `grok-4.5` | `high` (clamped) |
| `grok-4.5(none)` | `grok-4.5` | `low` (4.5 forbids zero) |
| `grok-4.3(none)` | `grok-4.3` | `none` (4.3 allows zero) |
| `grok-4.5(bogus)` | `grok-4.5` | `medium` (default; no error) |

The child owns the clamping table and never errors on an out-of-range value.

### Context-overflow error (GT-8)

697K tokens to `grok-4.5` (500K window) — HTTP 400:

```json
{"type":"error","error":{"type":"invalid_request_error","message":
 "{\"code\":\"invalid-argument\",\"error\":\"This model's maximum prompt length is 500000 but the request contains 620215 tokens.\"}"}}
```

### `max_tokens` has no ceiling to hit

`max_tokens` is **dropped in translation** — the upstream body carries
`max_output_tokens: null`. 65536, 65537 and 100000 all succeeded on both
`grok-4.5` and `grok-4.3`. The registry's 65,536 `max_completion_tokens` is
never exercised by an Anthropic-protocol request.

### Family-switch: a foreign thinking signature hard-fails

Contrary to the source reading that `sanitizeXAIInputEncryptedContent`
merely strips invalid content, a thinking block carrying a non-xAI
signature produced HTTP 400:

```
{"code":"invalid-argument","error":"Could not decrypt the provided encrypted_content. ..."}
```

Continuation variants, same history (the model must recall a number it
picked in turn 1):

| history shape | result |
|---|---|
| Grok's own signed thinking | recalled correctly |
| foreign (Claude-shaped) signature | **HTTP 400** |
| thinking block with `signature` removed | recalled correctly |
| thinking blocks dropped, text kept | recalled correctly |

The 400 quarantined the sandbox child's xAI credential
(`auth_unavailable` on the next call) until restart, which is how
a single bad request can look like an auth outage.

### Incidental

- A request for `grok-3-mini-fast` was forwarded as `grok-3-mini-fast` but
  answered by `grok-4.3`; `grok-4.5` answers as `grok-4.5-build`. Response
  `model` is not the requested slug, and routing is not identity.
- The child hot-loaded the copied auth files with no restart.

## Grok phase-5 harness measurements (2026-07-31, Claude Code 2.1.220)

Sandbox: second router (8898) + private CLIProxyAPI child (8398, `request-log:
true`) under `~/.claude/jobs/13cfa33f/tmp/phase5/`, credentials copied. Live
gateway untouched and re-verified healthy after.

### Driving Claude Code at a non-default gateway needs `--settings`

`~/.claude/settings.json`'s `env` block **silently overrides shell-provided**
`ANTHROPIC_BASE_URL`. Runs launched with a shell `ANTHROPIC_BASE_URL` went to
the *live* gateway instead, which reads as "the sandbox works" while
measuring nothing. `claude --settings <file>` with its own `env` block is the
reliable override.

### GATE 5 — foreign thinking signatures are NOT reachable

Phase 4 measured a hard 400 when a non-xAI thinking signature reaches Grok.
Driving the real harness shows Claude Code never sends one.

A genuine mid-session family switch (`-p` turn on `claude-sonnet-4-5`, then
`--resume` with `--model grok-4.5`) **succeeded**, recalling the number from
the Claude turn. The forwarded body carries:

```json
"context_management":{"edits":[{"keep":"all","type":"clear_thinking_20251015"}]}
```

and its 12-message history contains **zero thinking blocks** — the harness
strips them from replayed history itself. The reverse switch (Grok turn
first, then `--resume --model claude-sonnet-4-5`) also succeeded.

### WebSearch side call runs on a Claude small-fast model

The `WebSearch` side call runs on a **Claude small-fast model**
(`claude-haiku-4-5`) even in a Grok-main session, so it always arrives on the
**Claude branch**. The "Grok main" and "Grok subagent under Claude main"
topologies therefore exercise the same code path — the topology axis is not
independent.

Forwarding that sub-call to xAI **422s**:

```
Failed to deserialize the JSON body into the target type:
data did not match any variant of untagged enum ModelToolChoice
```

The sub-call carries Anthropic's server-side `web_search_20250305` tool and
its `tool_choice`; xAI cannot deserialize them.

### Effort is effective end-to-end (preliminary)

`grok-4.5`, one reasoning-heavy word problem, through the dev router (so the
model-ID suffix mapping is in play), n=2 per level:

| effort | latency | output tokens | reasoning chars |
|---|---|---|---|
| low | 42s, 59s | 3350, 4277 | 2282, 2716 |
| medium | 62s, 191s | 4932, 14148 | 2761, 7952 |
| high | 184s | 13656 | 7657 |

Reasoning volume and latency scale with the requested effort. Sample is
too small for pass-rate or per-tier guidance.

### Operational notes

- Repeated back-to-back Grok requests drove the sandbox child into
  `auth_not_found: no auth available (providers=xai)`; a child restart
  cleared it. Quarantine after upstream failures is the same behaviour
  recorded for Codex — one bad or rate-limited request can look like an auth
  outage.
- Neither the live nor the copied auth file was modified at any point
  (mtimes unchanged, same `expired` timestamp).

## Grok verification suite T1–T8 (2026-07-31, Claude Code 2.1.220)

Sandbox: third router (8899) + its own `CLIProxyAPI` 7.2.110 child (8399),
isolated `XDG_*` under `~/.claude/jobs/13cfa33f/tmp/suite/`, auth **copied**
from the live state dir (both live and copied files unchanged afterwards —
same mtimes). Live gateway untouched and re-verified healthy after.

24 cells, run twice (48 uncoached single attempts): all 8 tasks on
`grok-4.5` @ medium; the four diagnostic tasks (T1, T4, T5, T6) also @ low
and @ high; the same four @ medium on `grok-4.3` and on a **sandbox-only
hand-written `[[models]]` probe route** for `grok-4.20-0309-reasoning`.

### Driving effort from the CLI: `--effort` is the session-level control

`claude --effort <low|medium|high|xhigh|max>` sets effort for a `-p` session;
the capture tap confirms it arrives as top-level `output_config.effort`. An
`--agents` JSON block carrying `"effort"` did **not** take effect (the body
still carried Claude Code's default `medium`) — the flag is the reliable
channel. Claude Code sends `output_config.effort: medium` even when no
effort is requested, so "no effort" is not observable from the wire.

### Served models

`grok-4.5` is answered by **`grok-4.5-build`** in every response body
(the `-build` suffix is the only served-vs-named
divergence seen). `grok-4.3` and the `grok-4.20-0309-reasoning` probe are
each answered by their own name. The undocumented 4.20 snapshot is reachable
through a hand-written route with `family = "grok"` and needs no other
plumbing.

### Effort-effectiveness (grok-4.5, round 2, n=1 per cell)

| model | effort | cells | pass/soft/fail | mean wall | mean thinking chars | mean tool calls | tool errors |
|---|---|---|---|---|---|---|---|
| grok-4.5 | low | T1,T4,T5,T6 | 3/1/0 | 23.9 s | 638 | 6.2 | 1 |
| grok-4.5 | medium | T1,T4,T5,T6 | 4/0/0 | 28.9 s | 576 | 4.8 | 1 |
| grok-4.5 | high | T1,T4,T5,T6 | 3/1/0 | 16.4 s | 580 | 4.5 | 1 |
| grok-4.3 | medium | T1,T4,T5,T6 | 3/1/0 | 15.1 s | 762 | 8.2 | 12 |
| grok-4.20-0309-reasoning | medium | T1,T4,T5,T6 | 3/0/1 | 17.6 s | 697 | 6.5 | 4 |

Per task on `grok-4.5` (low → medium → high): T1 588/609/609 thinking chars,
T4 303/458/535, T5 57/142/139, T6 1606/1093/1038. **Effort produced no
monotonic effect on outcome, latency, or reasoning volume on these tasks** —
the only verdict spread (T6) does not order by effort. This is a
different regime from the phase-5 word problem, where reasoning volume and
latency scaled cleanly with effort: these tasks are agentic and easy, so
they do not separate the tiers. Any per-tier guidance needs harder tasks.

### Operational notes

- **No quarantine at all** across 48 runs with ~10 s of pacing between cells.
  Phase 5's `auth_not_found` came from back-to-back requests; a short gap is
  enough to avoid it.
- Every routed request returned 200 (plus the known `count_tokens` 404s).

## Grok-native WebSearch (2026-08-04, CLIProxyAPI 7.2.110, model-router 0.1.11)

xAI's hosted `web_search` tool, measured on a sandbox child generated from
the router's own `upstream_config_yaml` (isolated dirs, copied auth, live
gateway untouched).

### The wire shape (clean child, verified)

`POST /v1/responses` with `{"model":"grok-4.5","input":<query>,"tools":
[{"type":"web_search"}],"tool_choice":"required","stream":true,
"stream_tool_calls":true,"store":false,"temperature":0.1,"top_p":0.95,
"max_output_tokens":8192}`.

- `response.created` advertises the hosted-tool set **exactly**
  `[{"type":"web_search"}]` — no `x_search`. `tool_choice`, `temperature`,
  `top_p`, `store` and `max_output_tokens` are all accepted and echoed.
- `tools[0].filters.allowed_domains` works (5/5 harvested URLs on the
  requested domain). Excluded/blocked domains were never accepted upstream.
- Sources arrive on `response.output_item.done` where `item.type ==
  "web_search_call"`, in `item.action.sources[]` as `{"type":"url","url":…}` —
  **no titles**, so links render with the URL as their label.
- The **streamed** shape emits exactly one `web_search_call` item. The 7–9
  items in the phase-6 evidence are a non-streaming artifact, and their later
  source-bearing items repeat the first item's URL set exactly (0 unique URLs
  added), so waiting past the first item buys nothing.

### Closing the stream early quarantines the xAI auth

| action | next Grok request |
|---|---|
| read the stream to the end (10.6s) | 200 |
| three trivial completions back to back | 200, 200, 200 |
| **harvest at 4.9s, then close the connection** | **503 `auth_unavailable: no auth available (providers=xai)`**, 0 ms, no upstream call |
| probes after that abandon | 503 at +0s, 503 at +30s, 200 at +60s (a stacked burst stayed down ~4 min) |
| probe issued *while* a stream is still being drained | 200 |

So a client disconnect — not request volume — is what takes the xAI auth
offline, for 30–60s. Reading a stream to the end is measured not to block
concurrent Grok traffic. Verified live: with a search answered at 3.2s,
`SIGTERM` immediately after took **6.8s** to exit and the child logged the
search as `200 | 9.999s` — the stream ended by itself. Two searches back to
back, each followed immediately by another Grok request, produced no
`auth_unavailable` at all. Phase 4's `auth_not_found` note is superseded:
the child *process* survived, but its auth entry did not.

### Failed searches are visible (binary-verified, Claude Code 2.1.222)

The harness renders a `web_search_tool_result` whose `content` is not an array
as `` `Web search error: ${a.content.error_code}` ``, logged at error level and
pushed into what the model reads. xAI's own `grok-build` client emits the
same shape on failure (`{"type":"web_search_tool_result_error","error_code":
"unavailable"}`), and does not count a failed search toward
`web_search_requests`.

### Live end-to-end (sandbox router + real xAI)

One correlated run out of three came back with no `web_search_call` at all
despite `tool_choice: "required"`, and was reported as a failed search. It
overlapped an in-flight Grok turn; whether concurrency is the cause is not
established. Worth rechecking if users report spurious failures.

### Driving Claude Code at a non-default gateway no longer works (2.1.222)

The phase-5 recipe is dead: `--settings` with its own `env` block, a
project-level `settings.local.json`, `CLAUDE_CONFIG_DIR`, and an explicit
`ANTHROPIC_BASE_URL` in the environment were **all** ignored — every headless
run went to the user-level settings' gateway. `CLAUDE_CONFIG_DIR` does move
credential lookup (an isolated dir reports "Not logged in"), so it is read for
auth but not for the base URL.

## Grok 4.6 + CLIProxyAPI 7.2.132 (2026-08-14)

xAI released grok-4.6 on 2026-08-12 (500K window, effort low/medium/high/
xhigh, image input, a real model card). CLIProxyAPI's embedded registry
gained the ID in v7.2.131. The v7.2.132 release's official `checksums.txt`
matched sha256s computed from the downloaded artifacts on every platform.

CLIProxyAPI 7.2.110 → 7.2.132 tree diff (423 files, 215 production Go),
full-file diffs read for:

- codex identity (`codex_executor_request.go`: Originator pass-through +
  `codex-tui` default semantically identical to 7.2.110);
- the four xAI executor files (auth-error normalization only: 403
  bad-credentials remapped to 401 for refresh-retry; overflow error bodies
  pass through untouched);
- the thinking mapper (`internal/thinking/apply.go` — per-model level
  clamping from registry-declared levels: 4.6 declares `xhigh`, so
  `xhigh` stays and `max` → `xhigh` there, while 4.5 still clamps both to
  `high`).

Live verification, sandboxed instance (child :8318, auth-file copies):

| check | result |
|---|---|
| grok-4.6 smoke | answers as `grok-4.6-build` (the `-build` served-name convention carries over from 4.5) |
| grok-4.6 effort | `output_config.effort: xhigh` end-to-end success (suffix channel; per-model mapping verified in child source, not on the wire) |
| grok-4.6 overflow | 540087-token prompt → canonical `prompt is too long: 540087 tokens > 500000 maximum` — the xAI overflow phrase holds for 4.6 |
| Grok-origin WebSearch | sub-call answered from the xAI backend in 4.1s with real links (bun.sh results). The 2026-08-13 review-agent claim that Grok-native search feeds gzip to the harvester did **not** reproduce on 7.2.132 |

## Bundled `claude-api` skill and headless `--settings` (2026-08-18, Claude Code 2.1.235)

- GPT subagents follow skill trigger wording literally; the bundled
  `claude-api` skill triggers on any mention of Claude/Anthropic and its payload
  killed 17 subagents in one week ("Prompt is too long").
- Contradicting the 2.1.222 finding above: on 2.1.235 a `--settings` file
  with its own `env` block **does** override the user-level
  `ANTHROPIC_BASE_URL` for headless runs (verified by echoing the env and by
  the request landing in a second router instance's capture file).

## `modelPicker` vs the custom-model env pair (2026-08-25, Claude Code 2.1.246, router 0.1.15)

Context: 2.1.243 added a `modelPicker` setting ("curate the `/model` picker
with an ordered, labeled list of models … appended to or replacing the
built-in lineup").

Method: headless `claude -p 'say ok' --output-format json` runs from a scratch
dir (`--strict-mcp-config`, default auto mode, live gateway on :8787), each
under its own `--settings <file>` whose `env` block repeats the step-5 wiring
and either sets the pair or sets both vars to `""` — the consumers test
truthiness, so an empty string unregisters the slot even though the
user-level block still names it (verified: `printenv` in the run printed an
empty value, rc=0). Arms: pair only / neither / `modelPicker` only (sol with a
label, terra, luna, and `grok-4.6`, which this install does not serve) / both
/ `replaceBuiltInOptions: true`. Picker rendering was measured in real
interactive sessions driven through a pty (`pty.fork`, `TIOCSWINSZ` 140x45,
alt+p to open the picker, ANSI stripped from the captured bytes).
Consumption sites were read out of the 2.1.246 binary as before.

### (a) `--model <routing-id>` acceptance does not depend on either mechanism

| arm | `--model` | result |
|---|---|---|
| pair=sol | gpt-5.6-sol | ok |
| pair=sol | gpt-5.6-terra (unregistered) | **ok** |
| neither | gpt-5.6-terra | **ok** |
| neither | gpt-5.6-nope (unserved) | exit 1: "There's an issue with the selected model (gpt-5.6-nope). It may not exist or you may not have access to it." |
| picker | gpt-5.6-terra, gpt-5.6-luna | ok, ok |
| picker | grok-4.6 (row present, route disabled) | exit 1, same message as the unserved id |
| both | gpt-5.6-sol, gpt-5.6-terra | ok, ok |
| replace | sonnet, claude-fable-5 | ok, ok |

In `-p`, what decides is whether the gateway answers the first request; the
"issue with the selected model" text is the 404 `model_not_found` formatter,
not a registration check.
Every non-catalog id (the registered one included) now logs
`[claude-code:unrecognized_model] {"model":…,"query_source":"sdk"}` to stderr;
cosmetic. Interactive typed `/model <id>` (no pair, no picker): a served id is
accepted — and **Enter saves it as the default for new sessions, i.e. writes
`model` into the user settings file**; an unserved id fails validation with
"Model 'grok-4.6' not found". Binary: `/model` validation short-circuits for
alias names, the env-pair id, any `modelPicker` row (exact trimmed match,
after the allowlist/deprecation/entitlement filter), and previously
validated ids; anything else gets a 1-token probe request
(`querySource: model_validation`) through the gateway. A picker row therefore
skips the probe: an unserved row is selectable and fails at the first turn.

### (b) `CLAUDE_CODE_MAX_CONTEXT_TOKENS` is unaffected

`modelUsage[].contextWindow`: 258400 for every `gpt-5.6-*` run in every arm;
1000000 for `claude-sonnet-5` and `claude-fable-5` under the picker and
replace arms. Window resolution is still the `claude-` prefix rule.

### (c) The Agent tool's `model` enum stays closed

Binary (2.1.246): the Agent tool schema is still
`model: enum(["sonnet","opus","haiku","fable"]).optional()` — a hard-coded
literal, no reference to the picker. Measured under the picker arm: a Sonnet
main asked to call Agent with `model: "gpt-5.6-terra"` got
`InputValidationError … Invalid option: expected one of
"sonnet"|"opus"|"haiku"|"fable"`.

### (d) Setting shape, sources, interaction with the pair

```json
"modelPicker": {
  "options": [
    { "model": "gpt-5.6-sol", "label": "GPT-5.6 Sol", "description": "…" },
    { "model": "gpt-5.6-terra", "label": "GPT-5.6 Terra" }
  ],
  "replaceBuiltInOptions": false
}
```

- Rows need `model`; `label` and `description` optional. Default label is the
  built-in name for a known model, else the id; default description
  `Custom model (<id>)`. Invalid rows are dropped one at a time with a
  settings warning; a malformed field drops the whole key.
- **Sources: managed > `--settings`/SDK > user settings; project and local
  files are ignored.** Measured: rows placed in a scratch project's
  `.claude/settings.json` and `.claude/settings.local.json` did not render;
  `--settings` rows did. Binary: the lookup iterates exactly
  `["policySettings","flagSettings","userSettings"]` and takes the first
  defined value whole (the settings merger special-cases the key: higher
  source replaces, never merges). User-level placement itself was not
  exercised (the live file is off-limits for tests); it is the documented
  and binary-visible middle source.
- Both set (measured): the env-pair row renders first ("GPT-5.6 Sol · Custom
  model (gpt-5.6-sol)"), picker rows are appended after it in order. Binary:
  the append dedupes on model id (case-insensitive, `[1m]`-insensitive), so a
  picker row naming the pair's id is skipped in the list — but the display
  name used in the header/status line resolves the picker label *first*, so
  for a duplicated id the picker label wins there. Not measured.
- `replaceBuiltInOptions: true` (measured): only "Default (recommended)", the
  curated rows, and a raw row for the session's current model ("sonnet ·
  Custom model"); built-ins, gateway-discovered rows and the env-pair row are
  hidden (the last two per binary/docs).
- Labels are used beyond the picker: after selecting the Terra row for the
  session, the header read "GPT-5.6 Terra with xhigh effort · Claude Max" and
  the status line "GPT-5.6 Terra (xhigh)"; the toast still says "Model set to
  gpt-5.6-terra for this session only". Without a row the same places show
  the raw id.
- Server-side filtering ("Dropped: a row Claude Code can't serve") only
  covers first-party catalog cases (Fable/Mythos entitlement, rows the
  server marks disabled); a gateway id is never dropped or grayed, which is
  why the unserved `grok-4.6` row rendered.
- Managed-tier detail: when several admin tiers merge, `modelPicker` and
  `modelPicker.replaceBuiltInOptions` are stripped from every tier but the
  top one.

### (e) Rendering, measured

Picker arm, rows 6–9 after the five built-ins: "GPT-5.6 Sol (picker) · Codex,
general", "GPT-5.6 Terra · Custom model (gpt-5.6-terra)", "GPT-5.6 Luna ·
Codex, luna", "Grok 4.6 · Custom model (grok-4.6)" — order and labels exactly
as written. `s` (use for this session only) works on these rows; the ←/→
effort footer was shown but not exercised.

Still untested: user-level placement (see above); `availableModels`
interplay; a picker row duplicating the pair's id; managed settings;
behaviour on Claude Code < 2.1.242 (unknown-key handling); whether a picker
label affects anything on the wire (nothing in the binary suggests it — the
model string is sent verbatim).

## Subagent model-404 fallback (2026-08-29, Claude Code 2.1.251, router 0.1.15)

Context: the 2.1.247 changelog says "Fixed sub-agents dying on a first-call
model 404: they now use the session's fallback model chain, and the error
returned to the parent includes the error type, status, request id, and
model."

Method: headless `claude -p --output-format json --strict-mcp-config` runs
from a scratch dir (default auto mode, live gateway on :8787, the user-level
step-5 wiring), each asking the main session to spawn one custom agent
(`.claude/agents/*.md` with a `model:` line) and relay the Agent tool's
result verbatim. Which model actually answered was read from
`modelUsage` in the JSON result (authoritative; the agent's self-report is
not — see below). One interactive arm was driven through a pty. Consumption
sites were read out of the 2.1.251 binary.

| fallback chain | agent model | result | `modelUsage` besides the parent |
|---|---|---|---|
| none | gpt-5.6-nope (unserved) | `<error>Agent terminated early due to an API error: There's an issue with the selected model (gpt-5.6-nope). … (error type model_not_found, HTTP 404, request id req_…, model sent to the API: gpt-5.6-nope)</error>` | — |
| none | claude-nope-9 | same shape, 404 | — |
| none | claude-3-5-sonnet-20240620 (retired) | same shape, 404 | — |
| none | gpt-5.6-sol (control) | `MODEL-REPORT: gpt-5.6-sol` | gpt-5.6-sol |
| `--fallback-model sonnet` | gpt-5.6-nope | **`MODEL-REPORT: gpt-5.6-nope`, no error, no warning** | **claude-sonnet-5** |
| `--settings` file with `"fallbackModel": ["sonnet"]` | gpt-5.6-nope | same: silent, answered by Sonnet | **claude-sonnet-5** |
| `fallbackModel` setting, interactive (pty) | gpt-5.6-nope | agents view shows `Run nope-gpt report • claude-sonnet-5`; final text `MODEL-REPORT: gpt-5.6-nope`, no warning | (Sonnet, per the agents view) |
| `--fallback-model sonnet`, Workflow `agent(…, {model: "gpt-5.6-nope"})` | gpt-5.6-nope | `{"r":"MODEL-REPORT: GPT-5.6"}`, "0 errors" | **claude-sonnet-5** |

Findings:

- **Without a fallback chain nothing changed**: an unserved id still kills
  the subagent, and the parent now gets the richer error (type, status,
  request id, model). Same for the Claude branch (unknown and retired ids
  404 at api.anthropic.com through the gateway).
- **With a fallback chain the 404 pivots the subagent to the chain's first
  model, silently.** Neither the Agent tool result nor the Workflow return
  value carries any marker; the interactive agents view is the only place
  the served model is visible. The agent's self-report is worthless as a
  check — Sonnet answered "gpt-5.6-nope" (it echoes the agent definition)
  and "GPT-5.6" (Workflow arm).
- Binary (2.1.251): the pivot is one branch in the request retry loop —
  `if ((model_not_found || permission_denied || (!CLAUDE_CODE_RETRY_WATCHDOG
  && server_error)) && r.fallbackModel && r.fallbackModel !== r.model)
  throw new FallbackTriggeredError(...)`, logged as
  `tengu_api_model_not_found_fallback_triggered`. So the same chain also
  catches a **router 5xx** on a GPT subagent (Codex backend down, CLIProxyAPI
  503 after retries) — the flag's original "overloaded" purpose, now
  spanning providers. The subagent runner logs
  `tengu_api_subagent_model_not_found` with `has_fallback_chain:
  !CLAUDE_CODE_NO_MODEL_FALLBACK && options.fallbackModel` before throwing
  the no-chain error.
- Chain sources (`sJn` in the binary): CLI `--fallback-model a,b` (split on
  commas) else the `fallbackModel` settings array; max 3 entries;
  `"default"` expands to the default model; unknown ids are dropped
  (`kr(i)` catalog check — a GPT routing id in the chain is silently
  dropped, so the chain can only ever point at Claude). The same resolver
  feeds `prepared-interactive` and `prepared-headless`, and the pty arm
  confirms the setting is live in interactive sessions even though the
  `--fallback-model` help text still says "(only works with --print)".
  Settings merge: `fallbackModel` is "highest source that sets it owns it
  whole" (no array union), so a managed/policy `fallbackModel` overrides a
  user one, not the other way round.
- `CLAUDE_CODE_NO_MODEL_FALLBACK=1` disables every model substitution
  (`T6()`), with a tripwire that throws if a pivot is attempted. It also
  blocks the Fable-consent swap and compaction's
  substitution ("Compaction unavailable: CLAUDE_CODE_NO_MODEL_FALLBACK is
  set").

Retest triggers:
a changelog entry touching `fallbackModel`, `--fallback-model`,
`CLAUDE_CODE_NO_MODEL_FALLBACK`, or subagent model errors.

## Claude Code 2.1.257 review (2026-09-04, Claude Code 2.1.258, router 0.1.24)

Method: `claude -p … --output-format json | jq '.modelUsage'` through the
live tokened router (user settings: first-party flag,
`CLAUDE_CODE_MAX_CONTEXT_TOKENS=258400`), per-arm `--settings` JSON, plus
string extraction from the 2.1.258 binary. Request bodies were captured by
pointing one arm's `env.ANTHROPIC_BASE_URL` at a local stub that records
`/v1/messages` and answers a canned SSE stream.

### Fable 5.1

- `--model fable` and `--model best` resolve to `claude-fable-5-1`,
  contextWindow 1000000; `claude-fable-5` still served. The release note's
  "gateway sessions keep resolving to Fable 5" is a per-provider default
  (`fable: {default: "claude-fable-5-1", per_provider: {gateway:
  "claude-fable-5"}}`) for the enterprise gateway provider
  (`CLAUDE_CODE_USE_GATEWAY`), not for `ANTHROPIC_BASE_URL` proxies.
- Catalog: Fable 5.1 is `tier_10_50_cache_read_0_25` (cache read $0.25/Mtok
  vs $1 on Fable 5); API pricing only — subscription billing already
  discounted cache reads.
  New capability `fable_5_1_prompt_bundle`; `fallback_3p` is Fable 5.

### `CLAUDE_CODE_SUBAGENT_MODEL_FORCE`

- Sonnet main spawning `model-router:gpt-5.6-sol(medium)`: control
  `modelUsage` lists `claude-sonnet-5` and `gpt-5.6-sol`; with the variable
  set, `claude-sonnet-5` only — the agent ran on the subagent default, the
  request never reached the GPT branch, and nothing said so. Workflow logs
  `Workflow agent model "…" ignored: CLAUDE_CODE_SUBAGENT_MODEL_FORCE is
  set`; the Agent tool path has no equivalent.

### Gateway discovery (2.1.258 binary)

- Discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`) is skipped
  outright while `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` is set — the
  binary logs `[gatewayDiscovery] skipped: _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL
  is set` — and the flag is what keeps Claude models at 1M behind the router
  (200000 without it, re-measured on 2.1.258). Even with the flag off,
  discovery filters the gateway's list to IDs matching
  `/(claude|anthropic)/i`, so no `gpt-*`/`grok-*` route can
  appear through it, it reads only `id` + `display_name` (so it could never
  declare a window), and it fires only in interactive sessions. Retest if:
  bare Claude IDs get 1M behind a non-first-party base URL without the flag,
  or the discovery filter admits arbitrary IDs.
- The new `description` on discovered entries and the nonessential-traffic
  change land in the enterprise-gateway bootstrap path. `modelPicker` rows
  take `{ model, label?, description?, behavesAs? }`; `description` is the
  row subtitle, default "Custom model (<id>)".

### `behavesAs` on `modelPicker` rows (undocumented)

Schema text: "For a model this version of Claude Code does not know: the ID
of a model it does know whose client-side handling — prompt profile,
capability and effort defaults — applies to it. Changes neither the row's
label nor the model ID sent." Not in any changelog. Measured with
`{"model":"gpt-5.6-sol","behavesAs":"<target>"}` in `--settings`:

| target | window | max_tokens | thinking | `output_config.effort` (session xhigh) | `fallbacks` | system bytes |
|---|---|---|---|---|---|---|
| (none, control) | 258400 | 32000 | adaptive | xhigh | — | 6017 |
| claude-opus-4-8 | 1000000 | 64000 | adaptive | xhigh | — | 5888 |
| claude-opus-5 | 1000000 | 64000 | adaptive | xhigh | `"default"` | 9297 |
| claude-opus-4-7 | catalog 1000000 | 64000 | adaptive | xhigh | — | 27291 |
| claude-sonnet-5 | 1000000 | 64000 | adaptive | xhigh | — | 27291 |
| claude-fable-5 | catalog 1000000 | 64000 | adaptive | xhigh | `"default"` | 10239 |
| claude-fable-5-1 | 1000000 | 64000 | adaptive | xhigh | `"default"` | 12018 |
| claude-opus-4-6 | 200000 | 64000 | adaptive | high (clamped) | — | 27291 |
| claude-sonnet-4-6 | catalog 200000 | 32000 | adaptive | high (clamped) | — | 27291 |
| claude-sonnet-4-5 | catalog 200000 | 32000 | enabled, budget 31999 | dropped | — | 27416 |
| claude-haiku-4-5 | catalog 200000 | 32000 | enabled, budget 31999 | dropped | — | 27291 |

"window" is `modelUsage[].contextWindow` through the live router where
measured; "catalog N" rows were captured on the stub only. System bytes are
the joined `system` blocks of the first request. Only entries with
`lean_prompt` (opus-4-8, opus-5, the fables) get the ~6–12 KB prompt; the
rest carry the 27 KB one. Effort clamps to the target's ceiling
(`xhigh_effort` missing on opus-4-6/sonnet-4-6 → `high`); targets without
the `effort` capability drop it and send budgeted thinking.

- The window comes from the target's catalog entry and beats
  `CLAUDE_CODE_MAX_CONTEXT_TOKENS`; other routes keep the env value
  (per-route windows without scaling, for the two window sizes the catalog
  has: 200000 and 1000000).
- It follows the routing ID everywhere it is resolved, not only the picker:
  a project agent with `model: gpt-5.6-sol` and a Workflow
  `agent(…, {model: 'gpt-5.6-sol'})` both reported 1000000 with the row
  set; their `modelUsage` key becomes `gpt-5.6-sol[1m]` (suffix is
  client-side; the router still matched the bare route and the requests
  succeeded on Codex). `grok-4.6` with the row: 1000000, request succeeded
  on the xAI path.
- Target choice matters: opus-4-8 keeps the lean prompt, adaptive thinking
  and effort passthrough with no `fallbacks` field; opus-5 adds
  `fallbacks: "default"` and the refusal-fallback prompt section; sonnet-5
  and older sonnets pull in the 27 KB non-lean prompt, and pre-effort
  models drop `output_config.effort` and switch to budgeted thinking.
  `max_tokens` rises to 64000 for every 1M target.

### `behavesAs` on real open-weights routes (OpenRouter, same day)

Temporary `[[openai-providers]]` block: `moonshotai/kimi-k3` → `kimi-k3`,
`z-ai/glm-5.2` → `glm-5.2` (host windows 1048576 both; `verify-providers`
flags GLM's as 202752 guaranteed across sub-providers). Row under test:
`{"model": "<id>", "behavesAs": "claude-opus-4-8"}` via `--settings`.

- Direct gateway curl with `max_tokens: 64000`: both models answered
  (`stop_reason: end_turn`), so the 64000 the target implies is accepted
  by both hosts.
- `claude -p --model kimi-k3` / `glm-5.2`: contextWindow 258400 without the
  row, 1000000 with it; every run answered.
- 363K-token single prompt (24000 numbered lines, ~1.67 MB) on `kimi-k3`
  with the row: answered correctly (last line number and a word from line
  17777), `inputTokens` 363133, 35 s. Without the row: refused
  client-side in 140 ms, `"Prompt is too long"`, no request sent — the
  clipping is a hard wall, not a compaction nuisance.
- These runs prove the mechanism, not OpenRouter's routing: each request
  happened to land on a wide sub-provider. The window that matters is the
  one the host guarantees across every eligible sub-provider (Kimi K3
  1048576, GLM-5.2 202752 on an unpinned account).

### OpenRouter provider pinning: what the gateway can and cannot do (same day)

- OpenRouter has no API for account-level allowed/ignored providers; those
  live in the dashboard (`/settings/privacy`) and apply to every request.
  Request-level `provider` preferences exist: `only`, `ignore`, `order`,
  `allow_fallbacks`, `require_parameters`, …; a request's `only` narrows
  within the account list and ignores merge. Direct to OpenRouter,
  `provider.only = ["Nonexistent"]` fails with "No allowed providers are
  available for the selected model. Providers serving z-ai/glm-5.2-…:
  baidu, streamlake, deepinfra, …" — i.e. the list names *slugs*.
- Through the gateway, a top-level `provider` object in the Anthropic body
  (or under `metadata`) is dropped by CLIProxyAPI's translation: the
  request succeeded normally both ways.
- CLIProxyAPI's `payload` rules do reach the upstream body. Standalone
  child (7.2.132, port 8318) with one openai-compatibility model
  (`z-ai/glm-5.2` alias `glm-5.2`) and
  `payload.override-raw: [{models: [{name: "z-ai/glm-5.2", protocol:
  "openai"}], params: {"provider": "{\"only\":[\"Nonexistent\"]}"}}]`:
  the Anthropic-format request came back with OpenRouter's "No allowed
  providers" error, so the injected preference reached OpenRouter. The rule
  also matched by alias name, without `protocol`, and as a non-raw
  `override` with `"provider.only.0": "Nonexistent"`. A no-rule control
  answered normally.
- `/models/{id}/endpoints` lists every sub-provider with `provider_name`,
  `tag` (the slug `only` expects), `context_length` and
  `max_completion_tokens`; for GLM-5.2, 33 endpoints from 25 providers,
  windows from 202752 (Ambient) to 1048576.
- Standalone child pinned to `provider.only = ["fireworks","decart"]`:
  the `/v1/messages` response `id` is OpenRouter's generation id
  (`gen-…`), and `GET /api/v1/generation?id=` (after ~10 s) reports
  `provider_name: "Fireworks"`.
