# model-router: measured findings (2026-07-20, Claude Code 2.1.216)

Method: real Claude Code driver sessions (sonnet main) pointed at the router
(`ANTHROPIC_BASE_URL=http://127.0.0.1:8787`) with capture mode on, in two
phases: first with the GPT branch set to the built-in stub (sections that say
"stub boundary"), then against a real local CLIProxyAPI 7.2.90 + Codex OAuth
upstream (the Workflow measurements and "Live e2e results"). "Emitted" below =
observed in captured request bodies/headers.

## Answers to the design doc's open questions

### Dynamic general-purpose delegation (central workflow) — BLOCKED as specced
- The Agent tool's `model` parameter is a closed, harness-enforced enum
  (`sonnet | opus | haiku | fable`). Passing a GPT routing ID fails with
  `InputValidationError` before dispatch.
- `ANTHROPIC_CUSTOM_MODEL_OPTION` does not extend the enum.
- Gateway model discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`)
  never fired in `-p` sessions (zero `GET /v1/models` hits, no
  `~/.claude/cache/gateway-models.json`); untested interactively, but the enum
  is validated harness-side so extension is unlikely.
- The Agent tool has no `effort` parameter.
- Binary inspection (2.1.216 bundle): the enum is a hard-coded Zod literal at
  the Agent tool schema site with no env/flag/gateway conditional anywhere
  near it; the error is Zod's generic invalid_value formatter. Docs are
  silent on the per-invocation parameter's accepted values (the documented
  "full model ID" allowance is the *frontmatter/SDK* field, which works).
  The only documented override is `CLAUDE_CODE_SUBAGENT_MODEL` — session-wide,
  so unsuitable for mixed Claude+GPT sessions.

### Workflow tool — per-invocation model AND effort work (measured live)
Workflow scripts' `agent()` types `opts.model` as a plain string and skips
the Agent tool's enum validation entirely. Measured on 2.1.216:
- `agent(prompt, {model: 'claude-gpt-5.6-sol'})` ran the worker on the GPT
  routing ID (confirmed in capture) — no error.
- `agent(prompt, {model: 'claude-gpt-5.6-sol', effort: 'low'})` emitted
  `output_config.effort: low` on the worker's requests.
- `agent(prompt, {agentType: 'gpt'})` resolves a project agent definition and
  inherits its GPT model — the stable alternative if the string-typed
  `opts.model` ever gains validation.
So the design doc's full success criterion — dynamic per-invocation model +
effort without fixed agent files — is achievable today via Workflow
orchestration; only the bare Agent tool path is enum-blocked.

### Named-agent fallback — routing works through the router stub (adopted for MVP)
This section's runs used the stub boundary: they prove Claude Code's side
(agent spawn, model routing, effort emission, reply handoff). Tool-turn
continuation, tool search, and resume were subsequently verified against the
real upstream — see "Live e2e results"; long sessions and stop-reason edge
cases remain open (see "Still open").
A project agent definition with GPT model + effort frontmatter:

```yaml
---
name: gpt
model: claude-gpt-5.6-sol
effort: low
---
```

- Main session (sonnet) spawns `subagent_type: gpt`; every worker request
  carries the GPT routing ID through the router; the stub reply flows back
  normally. Parallel stub-GPT + real-Claude workers in one session work.
- Claude still *dynamically decides when* to delegate; on the Agent tool
  path the model-per-invocation knob is missing, so multiple agent files
  (e.g. `gpt`, `gpt-terra`) stand in for model choice there. The Workflow
  path (above) has no such limitation.

### Dynamic effort — SUPPORTED (better than the doc feared)
- `output_config.effort` IS emitted for unrecognized (GPT) model IDs, without
  `CLAUDE_CODE_ALWAYS_ENABLE_EFFORT` (docs imply otherwise — measured on
  2.1.216: session default "high" was sent).
- Agent frontmatter `effort: low` → worker request emitted `effort: low`.
- `CLAUDE_CODE_EFFORT_LEVEL=medium` → emitted `effort: medium`.
- CLIProxyAPI (v7.2.92, code-verified) maps `output_config.effort` →
  Codex `reasoning.effort`; gpt-5.6-sol/terra support low..xhigh+max+ultra.
- Effort control by API surface: Agent tool — per-agent (frontmatter) and
  per-session (env, `/effort`) only, no per-invocation parameter; Workflow
  `agent()` — full per-invocation `effort` (measured, see the Workflow
  section).

## Other measured facts
- Model-ID validation was not applied to the IDs we tested behind
  `ANTHROPIC_BASE_URL`: both `claude --model claude-gpt-5.6-sol` and the
  non-Claude-prefixed `claude --model gpt-5.6-sol` work with no
  custom-model env var. Not tested: malformed/arbitrary strings; setup
  should stick to the tested routing-ID patterns.
- `thinking: {type: adaptive}` is sent unconditionally to unrecognized model
  IDs (matches docs). With effort also emitted, CLIProxyAPI maps
  adaptive+effort → same-named `reasoning.effort`; adaptive WITHOUT effort
  would default to xhigh — effort emission (above) makes this a non-issue,
  but the router's GPT branch could add a guard later.
- OAuth capability beta string (must-measure item): `oauth-2025-04-20`, in an
  `anthropic-beta` list alongside `claude-code-20250219`,
  `interleaved-thinking-2025-05-14`, `effort-2025-11-24`,
  `context-management-2025-06-27`, etc. Router forwards the list verbatim on
  the Claude branch (E1: subscription OAuth session through router returned
  200 and billed to the subscription login), strips it on the GPT branch.
- Subagent system prompts are the agent-definition body only (plus a small
  billing-header block: `cc_is_subagent=true`); an honest GPT identity is
  trivially set in the agent file.
- Capture-mode nuance: on the stub path, records hold *incoming* headers
  (useful for measuring Claude Code); on real GPT forwarding, records hold
  the outbound (stripped/injected, redacted) set.

## CLIProxyAPI facts (codex research, v7.2.92, code-verified)
- Needs its own Codex OAuth (`cliproxyapi -codex-login`, browser +
  loopback:1455); no supported import of `~/.codex/auth.json`.
- `disable-claude-cloak-mode: true`; dedicated `auth-dir`; local `api-keys`
  secret → router's `gpt-upstream-api-key`.
- Translation is selective: tools/tool_choice/images/thinking translated;
  `cache_control`, `stop_sequences`, `metadata`, `max_tokens`, `defer_loading`
  stripped/dropped; SSE synthesized. Valid Codex slugs: `gpt-5.6-sol`,
  `gpt-5.6-terra`, `gpt-5.6-luna` (bare `gpt-5.6` is not a slug).

## Recommended settings (draft for setup skill)
```
ANTHROPIC_BASE_URL=http://127.0.0.1:<router-port>   # only this; no credential vars
ENABLE_TOOL_SEARCH=true   # tool search silently turns OFF behind a gateway otherwise
```
Leave unset: `CLAUDE_CODE_SUBAGENT_MODEL`, `ANTHROPIC_CUSTOM_MODEL_OPTION(_SUPPORTED_CAPABILITIES)`
(the latter is a documented no-op behind a gateway; the custom option is
unneeded because the tested routing IDs are accepted without it), `CLAUDE_CODE_ALWAYS_ENABLE_EFFORT` (effort
already emitted).

## Live e2e results (CLIProxyAPI 7.2.90 via Homebrew, Codex OAuth, gpt-5.6-sol)
- Direct completion through router → CLIProxyAPI → Codex: correct
  Anthropic-format response, model substitution and effort applied.
- Tool-turn continuation: GPT worker called Bash, consumed the result,
  finished the turn correctly.
- Tool search: with `ENABLE_TOOL_SEARCH=true` and genuinely deferred MCP
  tools (verified in capture: 2 deferred + ToolSearch present), the GPT
  worker used ToolSearch to load a deferred tool's schema and then called it
  successfully (single sample; agent prompt included a one-line ToolSearch
  explanation).
- Parallel GPT + Claude workers; Workflow orchestration with GPT
  participants; `-p --resume` multi-turn continuity — all worked.
- Cosmetic: the model self-reports as the routing ID ("Claude GPT-5.6 Sol");
  set identity in the agent definition if it matters.

## Production e2e (2026-07-20, v0.1.0 stack: managed CLIProxyAPI 7.2.92)

Measured against the production binary (managed mode, supervised child,
XDG-redirected root; real Codex OAuth reused from the earlier auth dir):

- Fresh-path flow: `doctor` → `ensure-upstream` (pinned download,
  vendored-checksum verify) → managed `serve` (port-free check, spawn,
  authenticated `/v1/models` readiness probe) all green; existing Codex login
  auto-imported from the legacy auth dir, no browser needed.
- Live sol completion through the managed stack: model substitution, response
  OK. Router-injected identity works: sol self-reports "GPT-5.6 Sol (Codex)",
  terra "GPT-5.6 Terra" (per-route display names).
- SSE streaming intact end-to-end.
- `count_tokens` on the GPT branch: kept as local 404 `not_found_error`
  (Claude Code falls back to local estimation; driver sessions unaffected).
- Crash recovery: SIGKILL on the child → detected, 1s backoff, respawned and
  ready again in ~1.3s.
- Claude-branch regression: subscription-OAuth `claude -p` session through
  the router works (billing to the login, as before).
- GPT agent tool-turn (Bash call + result consumption) works with the
  production agent body (no identity text, no ToolSearch hint).
- ToolSearch WITHOUT any prompt hint: gpt worker loaded a deferred MCP tool's
  schema via ToolSearch and called it successfully (second positive sample,
  first hint-free one). No patch needed yet; keep observing.
- Workflow `agent()` with `model: claude-gpt-5.6-terra` / `-luna` +
  `effort: 'low'` and a parallel Claude worker: all three returned correctly
  — every configured route verified live.
- Two defects found and fixed by this pass: a stale router process holding
  the port (better bind error now), and a supervisor hot-loop when the
  Supervisor was dropped without shutdown (watch-sender drop now treated as
  shutdown).
- Adversarial-review hardening, all re-verified live: ingress token (bare
  `/v1/messages` → 404; the `/t/<token>/` base URL from `doctor --json`
  completes against real Codex, and a `claude -p` driver session works
  through the tokened `ANTHROPIC_BASE_URL` on both branches); managed-mode
  startup failures serve degraded instead of exiting (Claude traffic
  unaffected); a live-but-unready child is terminated and respawned;
  graceful shutdown on SIGTERM reaps the child process group (observed on a
  real stop); the SessionStart hook's JSON validated with jq on every
  branch.

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
  tokens at `message_start` time. Fix directions: router GPT-branch SSE
  rewrite (estimated `input_tokens` in `message_start`, corrected in the
  final delta) or an upstream CLIProxyAPI patch; needs a harness-side test of
  which events Claude Code actually accumulates from.
- Context length (binary-verified, 2.1.216): the default context window for
  unknown model IDs is exactly 200000 (constant `EYt` in the window-sizing
  function). Undocumented override: `CLAUDE_CODE_MAX_CONTEXT_TOKENS` applies
  to any model whose normalized ID does NOT start with `claude-` (Claude
  models keep their built-in windows). Our `claude-gpt-5.6-*` routing IDs are
  excluded by the prefix check; bare `gpt-5.6-*` routing IDs (measured
  working earlier) would accept it. Tension: gateway model discovery silently
  drops IDs that don't start with `claude`/`anthropic`, so one routing ID
  can't have both the context override and a discovery picker entry —
  dual-route aliases (both prefixes → same upstream) would give each flow the
  right ID. Untested what happens if a conversation exceeds the real
  upstream limit. *Stale as of 0.1.12: the `claude-` alias routes were
  removed — the recommended setup disables gateway discovery
  (`_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL`) and uses the custom-model env
  pair for the picker, so only the bare routing IDs ship. The Claude Code
  facts recorded here (200K unknown-model default, `claude-` prefix
  exclusion) still hold; `claude-gpt-5.6-*` IDs elsewhere in this file are
  historical.*
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
  definitions the user does not control — chosen for setup step 5.
  `ANTHROPIC_BETAS=context-1m-2025-08-07` is a no-op: it reaches the wire but
  never the window calculation. The `/model` picker is unaffected either way —
  the server ships its Fable entry as `claude-fable-5[1m]` already
  (`additionalModelOptionsCache` in `~/.claude.json`), and subagents that omit
  `model` inherit the parent's suffixed string. The gap the flag closes is
  subagents with an explicit model: an agent defined `model: fable` measured
  200000 while its parent ran at 1000000 in the same session.
- GPT-branch safety of `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` (verified
  2026-07-28). Structural: the GPT branch calls `request_headers(...,
  strip_credentials = true, ...)`, which drops every credential header and
  `anthropic-beta` wholesale, so no beta the flag adds can reach the GPT
  upstream. Per behaviour: the 1M window is registry-gated and GPT IDs have no
  registry entry (measured: `gpt-5.6-sol` still reports 250000 with the flag
  on); refusal fallback is gated on the `refusal_fallback` model capability,
  which only `claude-opus-5`/`claude-fable-5` carry, and is not a wire
  parameter; the extra `auto-mode-classifier` beta applies only to auto-mode
  classifier queries, which run on a Claude model; the prompt-cache-scope beta
  was already being sent (`DH()` is true regardless of the flag) and the
  `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` sentinel does not leak into the GPT
  system prompt (asked the model directly). Not structurally blocked: non-beta
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
  InfoWorld 2026-07). Chosen `CLAUDE_CODE_MAX_CONTEXT_TOKENS=250000` to stay
  under that with margin. Revisit if Codex restores a larger window.
  (Superseded in 0.1.7: the declaration moved to the cap itself, 258400 —
  see the context-overflow translation section.)
- Display names (cosmetic UI question): two documented candidates behind a
  gateway — `ANTHROPIC_CUSTOM_MODEL_OPTION` + `_NAME`/`_DESCRIPTION` (single
  entry only), and `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` with a
  `/v1/models` endpoint returning `display_name` (picker labels entries
  "From gateway"). Binary-verified (2.1.216): discovery consumes ONLY
  `id` + `display_name` — mapped to `{value, label, description: "From
  gateway"}` for the /model picker; there is no context-length or
  capabilities field, so discovery cannot declare a model's context window.
  Discovery is gated on first-party auth + `ANTHROPIC_BASE_URL` + the env
  flag, and skipped when nonessential traffic is disabled; it never fired in
  `-p` sessions, consistent with an interactive-init-only fetch. Natural
  experiment: implement router `/v1/models` (forward + append GPT routes'
  id/display_name) and check the picker in one interactive session.

## Auto-compact accounting (binary-verified, 2026-07-27, Claude Code 2.1.220)

Method: symbol search and disassembly-adjacent string extraction from the
installed 2.1.220 binary. Motivated by the previous section's open question —
"needs a harness-side test of which events Claude Code actually accumulates
from" — because per-model context sizing depends on the answer.

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
- Consequence: rescaling the four reported usage fields moves the compaction
  trigger point, which is what `context-window-scaling` does. The unscaled
  post-anchor tail is in real tokens while the anchor is scaled, so a
  scaled-down route over-counts its tail slightly and compacts marginally
  early — the safe direction.
- Prefix rule re-verified on this version:
  `if (n !== undefined && n > 0 && !normalized.startsWith("claude-")) return n;
  return <200000 default>`.
- Also observed: `CLAUDE_CODE_AUTO_COMPACT_WINDOW` (clamped to [1e5, 1e6]) and
  an `autoCompactWindow` setting can *lower* the compaction window but never
  raise it above the model's max; both are global, so neither gives per-model
  sizing.
- Not verified: end-to-end behavior on a live scaled route against a real
  open-weights host (no provider key available). This is undocumented,
  reverse-engineered behavior — a Claude Code version that starts sizing
  context differently would turn scaling into silent overruns. *(Re-verified
  against 2.1.223 — see the context-window changes section below.)*

Externally sourced catalog facts (not measured here, 2026-07-27): GLM-5.2
advertises a 1M-token window under the separate `glm-5.2[1m]` model ID
(base ID serves less); Kimi K3 is listed at 1,048,576 on OpenRouter. Host-
served windows can be lower than the vendor's advertised number — the Codex
backend serving 272K of GPT-5.6's advertised 1.05M is the same pattern — and
OpenRouter's `context_length` is the maximum across its sub-providers.

## Context-window changes in Claude Code 2.1.223 (binary-verified, 2026-08-06)

Method: string extraction from the installed 2.1.223 binary, same approach as
the 2.1.220 dig above, which this satisfies the retest trigger of. Changelog
context: 2.1.223 changes `CLAUDE_CODE_DISABLE_1M_CONTEXT` behavior, enforces
assumed context limits on unrecognized model IDs, and adds a startup warning
around both.

Core assumptions re-verified byte-for-byte equivalent — scaling remains valid:

- Prefix rule unchanged: `CLAUDE_CODE_MAX_CONTEXT_TOKENS` applies exactly when
  the resolved model ID does not start with `claude-`; otherwise the 200K
  unknown-model default. Routed IDs still get the declared 258400.
- Gate arithmetic unchanged: anchor = most recent usage-bearing assistant
  message; sum of exactly `input_tokens + cache_creation_input_tokens +
  cache_read_input_tokens + output_tokens` (`yHe`); client-estimated unscaled
  tail after the anchor; 20K output reserve; 13K compact margin.

New in 2.1.223, and how it interacts with the router:

- Window resolution (`v9`) now tags a source. Unrecognized model IDs — every
  routed ID — resolve as source `"unknown-model"` (window still the
  `CLAUDE_CODE_MAX_CONTEXT_TOKENS` value); previously they fell through to
  `"auto"`. The auto-compact gate (`hky`) short-circuits to *disabled* when
  the source is `"auto"`, so this change is what makes the tokens gate firmly
  cover routed models. Net effect for the router: auto-compact on routed
  models is now guaranteed by an explicit code path rather than incidental.
- **Footgun:** `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1`
  ("restores the previous wait-for-the-API behavior") reverts routed models
  to source `"auto"` — the tokens gate never fires, sessions grow until the
  router's context-overflow translation backstop trips at the Codex cap.
  Works, but degraded UX. Do not set it alongside the router.
- The new unrecognized-model startup notice ("X is not a model this version
  of Claude Code recognizes…") is suppressed exactly when the model is
  non-`claude-` and `CLAUDE_CODE_MAX_CONTEXT_TOKENS` > 0 — the setup skill's
  declaration keeps router sessions notice-free. Without the declaration the
  notice appears and the assumed window is 200K.
- `CLAUDE_CODE_DISABLE_1M_CONTEXT` now clamps native-1M Claude models to 200K
  via auto-compact, and emits a startup warning for any model whose window
  exceeds 200K with source ≠ `"auto"` — which includes every routed model
  (258400 > 200K). A user setting that var sees "the 200K limit isn't
  enforced for gpt-5.6-sol…" suggesting `CLAUDE_CODE_AUTO_COMPACT_WINDOW=
  200000`. Cosmetic (the routed window is intentionally above 200K), but the
  copy reads as a misconfiguration.
- `CLAUDE_CODE_AUTO_COMPACT_WINDOW` / `autoCompactWindow` semantics unchanged
  (lower-only, clamped to [100K, 1M], global). On a scaled route the
  real-token trigger shrinks proportionally: `configured × actual/declared`.
  Compacts early — the safe direction.
- Account state `longContext1mCreditsBlocked` (set when the API refuses 1M
  for credit reasons) clamps *any* model with a resolved window above 200K to
  200K — routed models included. The router's 258400 declaration is then
  optimistic; scaled usage over-reports against the clamped window and
  compaction fires early. Safe direction, no action.
- `modelOverrides` (the notice's suggested remedy) is a model *aliasing* map,
  not per-model windows — no use to the router. Alias resolution runs before
  the `claude-` prefix check, so mapping a routed ID to a `claude-*` name
  would strip the env-var window. Also retroactively validates 0.1.15's
  removal of the `claude-` alias routes: those IDs ignore the declaration
  entirely.

## GPT tool-usage audits (2026-07-20, 5 transcripts, Sonnet auditors)

Five probe tasks (sol x3, terra, luna) audited from raw transcripts. Task
completion was 5/5 and output accuracy verified; recurring frictions:

- Bash `timeout` parameter hits a `timeout:*` permission-deny rule (3/5
  transcripts); always recovered in one retry. Decide: allow the parameter
  or leave as-is.
- Bash `rg`/`find` instead of Grep/Glob (sol once — caused two
  output-overflow truncations on `.git/`; terra heavily; luna mildly).
  Countermeasure: built-in-tools nudge added to all GPT agent bodies.
- Final-message semantics (terra): produced a complete, sound review as a
  mid-run text block, then let a late event push it to a stub final message —
  the caller only receives the final message, so the review was lost.
  Countermeasure: final-message nudge added to all GPT agent bodies.
- Terra also self-forked a duplicate investigator on the same task (2x
  compute, ignored the tool result's do-not-duplicate warning) and misparsed
  its own Skill args ("Review …") as a PR branch name, then fetched local
  files via `gh api` instead of `git show`/Read. Single sample; observe.
- Luna is strictly serial: 27 calls, one per turn, no parallel batches on an
  8-way-parallel task (sol batches well). Accuracy was still 100%.
- Harness note for callers: subagent Writes of report/findings `.md` files
  are hard-blocked by Claude Code ("return findings as text"); ask GPT (and
  any) subagents for inline findings, not report files.

Round 2 (2026-07-21, 20 probes: 5 tasks x {sol-med, sol-low, terra-high,
luna-high}, Sonnet auditors + blind comparer): 20/20 task success, 20/20
complete final messages, 0 self-forks, 0 hallucinated constraints — round
1's terra final-message loss did not reproduce. Effort did not predict
fumbles (sol-low was the cleanest and leanest: 54 tool calls vs sol-med 82,
luna 112). n=1 incidents, tracking model family not effort: terra operated
on its worktree cwd instead of the prompt-named path; luna wrote a file
outside its assigned directory via a path bug and did not disclose it;
sol-med satisfied a no-file-writes task via a disk-caching fetch tool.
All-config artifact, not a model signal: the bundled claude-api skill's
imperative trigger fires on any "Claude Code" mention in a prompt, costing
a turn each time (once 784KB of context) — our probes were meta-tasks about
Claude Code itself; typical research prompts won't mention it. Decision:
keep sol, terra, and luna all recommended; no choosing-models copy changes
(the observed failure modes are too incidental to teach from).

Review-quality bake-off (2026-07-21, blind: sol-med/sol-high/terra-high each
reviewed commits 9fad035 and f14896e; Sonnet judge with repo access verified
claims before ranking). Result: sol-high 1st on both (found the one real,
judge-verified bug in each commit with the tightest grounding, no padding);
sol-med 2nd (same bugs, some speculative filler); terra-high 3rd — approved
both commits with ~400-char reviews, missing the real bug both times.
Transcript check: terra's tiny reviews were its entire written output (one
text block each), not a lost-final-message artifact, despite heavy
investigation activity. Steinberger's terra-for-review recommendation does
not replicate through this integration. Action: reviewer agents switched to
sol at high effort. Bonus: the bake-off surfaced two real repo bugs — the
starfield docH self-pinning feedback loop (9fad035) and the hive fork-hook
registration skip for first-seen directories (f14896e).

### Correction: Grep/Glob did not exist in the audited sessions (2026-07-28)

Claude Code 2.1.117 removed the standalone Grep/Glob tools on native
macOS/Linux builds — search moved to embedded `bfs`/`ugrep` via Bash
(2.1.162 restores the tools only when `--tools` names them explicitly).
The audits above ran on 2.1.217, so "Bash `rg`/`find` instead of
Grep/Glob" was the models using the only search path available, and the
`bash_over_dedicated` rubric penalized tools absent from the request.
The router-side nudge shipped as countermeasure pointed GPT models at
Grep/Glob; 0.1.6 cuts the whole "prefer the dedicated tools … over
shell equivalents" clause — the cat/sed-vs-Read half of the audit
finding is already stated verbatim in the Bash tool description every
request carries — keeping only the trained-harness-differs /
read-descriptions-closely warning. Last transcript on this machine
with a real Grep/Glob call: 2026-04-27, v2.1.119.

Verified in a live Bash session (2.1.220): plain `find`/`grep` are
shadowed by shell-snapshot functions re-execing the `claude` multicall
binary as bfs/ugrep, the `grep` shim defaulting to `--ignore-files
--hidden -I --exclude-dir=.git` (+ .svn/.hg/.bzr/.jj/.sl) — which would
have prevented the audit's `rg --hidden` overflow on `.git/`. `rg`
resolves to the user's real ripgrep when installed, so `--hidden`
sweeping `.git/` remains possible there. Shell search is the sanctioned
path; there is nothing to nudge GPT models away from.

## Effort and windows on openai-compatibility upstreams (2026-07-28, CLIProxyAPI 7.2.92)

Method: ran the cached CLIProxyAPI binary against a local fake
OpenAI-compatible host that logs the exact body it receives, with one
`openai-compatibility` provider configured. No provider account involved.

- `output_config.effort` **is** forwarded, as OpenAI's top-level
  `reasoning_effort`, verbatim: low/medium/high/xhigh/max each arrived
  unchanged. The previous skill claim that open-weights routes ignore effort
  was wrong (and had never been measured).
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
endpoint lists `reasoning_effort` in `supported_parameters`.

Provider pinning is only available two ways, both outside the request the
router controls: the per-request `provider.only`/`order` fields (CLIProxyAPI
builds that body, so the router cannot inject them) and account-wide
allowed/ignored providers at `https://openrouter.ai/settings/privacy`, which
merge with the per-request lists and apply to every API call. There is no
model-slug pin — `:nitro` and `:floor` only change sorting.

Consequence: discovery takes the narrowest endpoint on OpenRouter, which for
this model means it declines to scale rather than promising 1M. Pinning
account-side (then setting `context-window` by hand, since the catalog the
router reads is unaware of account settings), or using a direct host, is what
makes the advertised window real.

## Per-session WebSearch budget (2026-07-29, Claude Code 2.1.220)

Observed during the web-research bake-off (not documented anywhere we
found): a Claude Code session has a ~200-call WebSearch budget shared
across the main loop, subagents, and Workflow agents. Once exhausted,
WebSearch calls return a budget-exhausted notice ("200 of 200 WebSearch
calls") and agents silently degrade to WebFetch-only — this invalidated a
36-agent judging round mid-run before it was noticed; the only in-band
signal is the notice in each affected agent's transcript. Fresh `claude
-p` sessions each get their own budget, so search-heavy multi-agent
evaluations should run searchers as independent `-p` sessions (the
bake-off's rescue scripts follow this pattern). Undocumented harness
behavior — recheck the cap and its scope on Claude Code upgrades.

## Still open
- Long sessions and heavy tool loops against real Codex; more ToolSearch
  samples (single positive so far).
- Gateway model discovery in interactive sessions (cosmetic: /model picker
  entries; not needed for the agent-file or Workflow flows).
- Compliance: user reviewed the subscription-OAuth consideration and accepted
  the risk (2026-07-20); revisit before any public release of the plugin.
- The Workflow `opts.model` string-typing is an implementation detail, not a
  documented contract — retest on Claude Code upgrades.

## WebSearch on GPT main-loop sessions (2026-07-21, Claude Code 2.1.217, v0.1.4)

- Mechanism (2.1.217 bundle): the WebSearch tool issues a side `/v1/messages`
  call on the session's **main-loop model** whose only tool is the
  server-side `web_search_20250305` tool, then parses links out of
  `web_search_tool_result` blocks; a result block with empty `content`
  renders as "No links found." (A statsig gate, `tengu_plum_vx3`, would
  switch the side call to the small-fast model; observed off.) The bundle
  source sets `tool_choice: {type: tool, name: web_search}`, but the live
  captured request carries `tool_choice: auto` — detection accepts both,
  keying on the prefixed "Perform a web search for the query: " user message
  and the tools list containing only the server tool.
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
- v0.1.4 therefore intercepts the recognized sub-call shape on the GPT
  branch and answers it from `/v1/alpha/search` via the same CLIProxyAPI
  child; on failure it falls back to the buffered LLM call with links
  scraped from the text into the empty result block. `[web-search]
  mode = "alpha" | "scrape" | "off"` in the config.
- Live e2e (2026-07-21): `claude -p --model gpt-5.6-sol` WebSearch through
  the intercepting router returned a fully populated Links array in
  ~2s; identical query on the passthrough router returned "No links found."
  after 20–70s. `/v1/alpha/search` is an undocumented endpoint — retest on
  Codex/CLIProxyAPI upgrades.

## Origin-matched WebSearch backend (2026-07-22, follow-up on the above)

- The sub-call always runs on the session's **main-loop model** even when a
  subagent invoked WebSearch (verified by capture: gpt-5.6-sol subagent under
  a haiku main produced a Claude-branch sub-call on haiku). The sub-call
  carries `cc_is_subagent=true` in its billing-header block but nothing
  identifying the requesting agent's model.
- The router therefore correlates: it passively taps `/v1/messages` responses
  of requests that declare the client `WebSearch` tool, watches for the
  `WebSearch` tool_use (SSE `input_json_delta` accumulation), and records
  `(session_id, query, domains) → origin model` before the completing event
  reaches the client. The follow-up sub-call consumes the entry and routes to
  the origin-matched backend: GPT origin → `alpha/search` (or the LLM+scrape
  path in `scrape` mode), Claude origin → Anthropic native.
- Claude-origin sub-calls arriving on the GPT branch are re-issued to
  Anthropic as a **normalized** request (allowlisted fields, origin model,
  `max_tokens` clamped, main-model tuning fields like `output_config`
  dropped, inbound OAuth + `anthropic-beta` preserved, no identity block),
  buffered non-streaming and re-framed. 400/401/403/404 from Anthropic are
  surfaced unchanged; transport/5xx failures fall back to the GPT path.
- Correlation is heuristic: identical concurrent queries in one session
  consume FIFO; a miss (TTL 120s, restart, main-loop search) degrades to the
  previous main-model-matched behavior.
- Live e2e (2026-07-22): haiku main + gpt-5.6-sol subagent → subagent's
  search answered from `alpha/search`; gpt-5.6-sol main + haiku subagent →
  subagent's search answered by Anthropic native with a populated Links
  array. Both verified in router logs and session transcripts.

## Context-overflow translation (2026-07-29, Claude Code 2.1.220 + codex-rs HEAD 6493417150 + CLIProxyAPI 7.2.92)

Motivated by a live failure: a workflow-heavy session on `gpt-5.6-sol` hit
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

### Router fix (0.1.7): translate to Anthropic's canonical error

`crate::overflow` rewrites the message to `prompt is too long: N tokens >
M maximum` on both wire shapes — emulating Anthropic's public error surface,
not any client's internals. `M` = the route's real window
(`GPT_CONTEXT_WINDOW` raised 250000 → 258400, now load-bearing); `N` = the
router's o200k input estimate (computed lazily for non-streaming requests),
clamped to `max(N, M+1)` because the estimator ignores non-text content and
a false `N ≤ M` would starve gap-guided compaction. Scope: Codex-native
upstream models only (`gpt-5.6-sol/terra/luna`) — the matched phrase is
verified for that backend alone, and a false positive elsewhere would
re-create the retry loop this removes. Detection requires
`invalid_request_error` plus whitespace-normalized, case-insensitive
`input exceeds the context window` (subject-bearing: a `max_tokens ...`
variant must not match). GPT-branch forwards now always request identity
encoding (loopback traffic; response bytes must be parseable).
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` moves to the cap itself, 258400 (setup
skill, `DEFAULT_DECLARED_CONTEXT_WINDOW`, config template): with recovery
restored the declaration is an efficiency dial, and the 238.4K gate under
the 258.4K cap keeps 20K of preventive slack — Claude Code's own output
reserve, and still more headroom than Codex's 13.6K. Existing scaled
open-weights routes with windows in [250000, 258400) would fail validation
under the raised declaration; setup must check configured provider windows
before rewriting the value (open-weights.md documents the thresholds).

Live e2e (2026-07-29, patched router in external mode against the running
CLIProxyAPI child): oversized requests on `gpt-5.6-sol` and
`claude-gpt-5.6-sol`, streaming and non-streaming, all four returned
`prompt is too long: 1680090 tokens > 258400 maximum` (real lazy estimate on
the non-streaming path). **Retest triggers:** any
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
rollout in progress (openai/codex#32806 anticipated a restore).
`GPT_CONTEXT_WINDOW` stays 258400 deliberately: it is the *documented*
served limit, the translated `M` only guides compaction sizing (a
conservative M over-compacts slightly, the safe direction), and chasing an
in-flight rollout would bake in a number measured on one evening. Retest
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

Fix (0.1.8): the GPT branch sets `Originator: codex_cli_rs` in
`headers::request_headers`, overriding any inbound value. Watch for the
backend tightening the fingerprint check (e.g. requiring a matching
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

### Reasoning effort does NOT survive on the xAI path (decides D12)

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

The child owns the clamping table and never errors on an out-of-range value,
so the router forwards the requested effort unvalidated.

**Router fix:** `routing::effort_qualified_model` appends the suffix for
Grok-family routes only. Verified end-to-end through the dev router:
low/medium/high each arrive as the matching `reasoning.effort`. Bare routes
and the existing agent-frontmatter channel are preserved — no suffixed
routes, no new agent files, no user-visible concept.

### Alpha-search model pin, live (D7.1)

> Still true for GPT and open-weights origins. Grok origins no longer reach
> `/v1/alpha/search` at all (2026-08-04, see "Grok-native WebSearch").

A `WebSearch` sub-call on a `grok-4.5` route reached `/v1/alpha/search` and
returned **33 structured links**. The payload carried the pinned Codex slug,
not the Grok one:

```
{"model":"gpt-5.6-sol","q":"rust axum graceful shutdown"}
```

### Context-overflow error (GT-8): translatable

697K tokens to `grok-4.5` (500K window) — HTTP 400:

```json
{"type":"error","error":{"type":"invalid_request_error","message":
 "{\"code\":\"invalid-argument\",\"error\":\"This model's maximum prompt length is 500000 but the request contains 620215 tokens.\"}"}}
```

Subject-bearing and unambiguous, so `OverflowRewrite` is now armed for the
built-in Grok models via a second dialect phrase (`maximum prompt length
is`). The Codex arming rule is unchanged.

### `max_tokens` has no ceiling to hit

`max_tokens` is **dropped in translation** — the upstream body carries
`max_output_tokens: null`. 65536, 65537 and 100000 all succeeded on both
`grok-4.5` and `grok-4.3`. The registry's 65,536 `max_completion_tokens` is
never exercised by an Anthropic-protocol request, so no clamp is needed.

### Identity block reaches xAI (gate 6)

The injected block arrives as a `developer` message in `input[0]`:

> You are Grok 4.5, a Grok model working inside Claude Code's agent
> harness alongside Claude models. Do not present yourself as Claude. ...

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

So a mitigation exists (drop the signature) and costs nothing measurable on
a short session. **Not implemented**: whether Claude Code actually replays
another family's thinking blocks on a mid-session `/model` switch is a
harness question, not a wire question, and shipping a body rewrite for an
unconfirmed trigger would be speculative. Phase 5 must answer it — see the
verification suite. Also note the 400 quarantined the sandbox child's xAI
credential (`auth_unavailable` on the next call) until restart, which is how
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
`ANTHROPIC_BASE_URL` (the setup skill already warns about this for smoke
tests). Runs launched with a shell `ANTHROPIC_BASE_URL` went to the *live*
gateway instead, which reads as "the sandbox works" while measuring nothing.
`claude --settings <file>` with its own `env` block is the reliable override;
`ANTHROPIC_CUSTOM_MODEL_OPTION` in that block is also what makes `--model
<routing-id>` accepted (an unregistered ID is rejected with "issue with the
selected model", regardless of `/v1/models` discovery).

### GATE 5 — foreign thinking signatures are NOT reachable (no mitigation needed)

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

**Verdict: the phase-4 wire failure is unreachable through Claude Code. No
router-side signature stripping is warranted** — it would be a body rewrite
for a trigger the harness prevents.

### WebSearch: the 2x2 collapses to a 1x2, and cell 4 does NOT land on Grok

> **Superseded for Grok origins (2026-08-04).** Both rows below describe the
> pre-0.1.11 behaviour. Grok-origin searches no longer reach `alpha/search`
> and no longer fall back to Anthropic; they run on xAI's own hosted
> `web_search` and fail visibly when they cannot. The recommendation this
> section ends with is resolved: D7.5 was funded, via `web_search` rather than
> `x_search`. See "Grok-native WebSearch" below. The measurements stay as the
> record of what the alpha path did.

The `WebSearch` side call runs on a **Claude small-fast model**
(`claude-haiku-4-5`) even in a Grok-main session, so it always arrives on the
**Claude branch** and is matched to its origin by the correlation tap. The
"Grok main" and "Grok subagent under Claude main" rows therefore exercise the
same code path — the topology axis is not independent.

| alpha | observed |
|---|---|
| up | `answered routed-origin web search from alpha/search links=12..40 origin="grok-4.5"`, payload `{"model":"gpt-5.6-sol"}` (the D7.1 pin) |
| down (codex auth removed) | alpha 503 → **falls back to the origin route's scrape path** (the D7.2 fix fires) → that forward **422s** → passes through to Anthropic |

The 422 body:

```
Failed to deserialize the JSON body into the target type:
data did not match any variant of untagged enum ModelToolChoice
```

The sub-call carries Anthropic's server-side `web_search_20250305` tool and
its `tool_choice`; xAI cannot deserialize them. So **the required cell-4
behaviour (land on Grok `legacy_websearch`) is not achievable as-is**, and
the plan's assumption that the scrape path is a usable Grok fallback is
wrong.

Stripping the tool to stop the 422 is **not** an improvement worth making:
without the tool the request is a plain completion and Grok has no web
access, so it would answer a search query from training data — the suite's
own T7 fail signal is a fabricated URL. Anthropic answering correctly is
strictly better for the user than Grok guessing. The principled fix is xAI's
native `x_search` (plan D7.5), still out of scope. **Recommendation for
Fable/the user:** either accept and document the Anthropic fallback for
Grok-origin searches when alpha is unavailable, or fund D7.5. [copy: Fable]

### Effort is effective end-to-end (preliminary)

`grok-4.5`, one reasoning-heavy word problem, through the dev router (so the
model-ID suffix mapping is in play), n=2 per level:

| effort | latency | output tokens | reasoning chars |
|---|---|---|---|
| low | 42s, 59s | 3350, 4277 | 2282, 2716 |
| medium | 62s, 191s | 4932, 14148 | 2761, 7952 |
| high | 184s | 13656 | 7657 |

Reasoning volume and latency scale with the requested effort, confirming the
phase-4 fix works in practice and that effort is worth exposing. Sample is
too small for pass-rate or per-tier guidance; the full suite matrix is still
outstanding (see below).

### Operational notes

- Repeated back-to-back Grok requests drove the sandbox child into
  `auth_not_found: no auth available (providers=xai)`; a child restart
  cleared it. Quarantine after upstream failures is the same behaviour
  recorded for Codex — one bad or rate-limited request can look like an auth
  outage.
- Neither the live nor the copied auth file was modified at any point
  (mtimes unchanged, same `expired` timestamp).

## Grok verification suite T1–T8, full matrix (2026-07-31, Claude Code 2.1.220)

Sandbox: third router (8899) + its own `CLIProxyAPI` 7.2.110 child (8399),
isolated `XDG_*` under `~/.claude/jobs/13cfa33f/tmp/suite/`, auth **copied**
from the live state dir (both live and copied files unchanged afterwards —
same mtimes). Live gateway untouched and re-verified healthy after.

24 cells, run twice (48 uncoached single attempts): all 8 tasks on
`grok-4.5` @ medium; the four diagnostic tasks (T1, T4, T5, T6) also @ low
and @ high; the same four @ medium on `grok-4.3` and on a **sandbox-only
hand-written `[[models]]` probe route** for `grok-4.20-0309-reasoning`.
`grok-3-mini-fast` was dropped (phase 4 saw it answered by another model).
Every cell was scored by fresh Claude auditors against the suite rubric,
independent of the executor.

### Driving effort from the CLI: `--effort` is the session-level control

`claude --effort <low|medium|high|xhigh|max>` sets effort for a `-p` session;
the capture tap confirms it arrives as top-level `output_config.effort` and
the router rewrites it to the `model(effort)` suffix on the xAI path. An
`--agents` JSON block carrying `"effort"` did **not** take effect (the body
still carried Claude Code's default `medium`) — the flag is the reliable
channel. Claude Code sends `output_config.effort: medium` even when no
effort is requested, so "no effort" is not observable from the wire.

### Served models

`grok-4.5` is answered by **`grok-4.5-build`** in every response body
(annotate accordingly; the `-build` suffix is the only served-vs-named
divergence seen). `grok-4.3` and the `grok-4.20-0309-reasoning` probe are
each answered by their own name. The undocumented 4.20 snapshot is reachable
through a hand-written route with `family = "grok"` and needs no other
plumbing.

### Results (round 2 — isolated scratch dirs; auditor verdicts)

| Task | grok-4.5 low | grok-4.5 medium | grok-4.5 high | grok-4.3 med | grok-4.20 med |
|---|---|---|---|---|---|
| T1 exact-match edit | PASS | PASS | PASS | PASS (demerit) | PASS |
| T2 bash output | — | PASS | — | — | — |
| T3 grep/glob | — | PASS | — | — | — |
| T4 multi-step fix | PASS | PASS | PASS | PASS (demerits) | PASS (demerits) |
| T5 identity/format | PASS | PASS | PASS | PASS | PASS |
| T6 degeneracy | SOFT | PASS (contaminated) | SOFT | SOFT | **FAIL** |
| T7 WebSearch | — | PASS | — | — | — |
| T8 long context | — | SOFT | — | — | — |

Round 1 (same cells, before the scratch-isolation fix) agreed everywhere
except T6, where 4.5-low was SOFT, 4.5-medium and 4.5-high PASS, 4.3 SOFT,
4.20 FAIL — i.e. only the low/high T6 verdicts moved, and the 4.20 fabrication
reproduced in both rounds.

- **T1** — every cell fixed the single line, `tests`-style byte-compare clean,
  no full-file `Write`, zero failed Edits, tab-indented function preserved.
  `grok-4.3` needed 5 extra calls recovering from a path it mangled (below).
- **T2** — reported mean/exit/skip exactly matched the real Bash result block
  (59.50 / 3 / 2) and `results.txt` carried them.
- **T3** — `rg -n -w` word-boundary search; ground-truth file/line map matched
  exactly, plural decoys excluded, comment-only reference identified.
- **T4** — all five cells ran pytest before their first edit, left `tests/`
  byte-identical, ended on a real `6 passed` result block, and summarized both
  seeded bugs correctly; exactly one edit/test cycle each.
- **T5** — identity held everywhere: "I am Grok 4.5 … created by xAI",
  "Grok 4.3 … created by xAI", "Grok 4.20 … created by xAI". No cell claimed
  to be Claude or GPT; one thinking block framed itself as "working inside
  Claude Code's agent harness", which is the allowed framing. Zero tool calls
  in all five, lists alphabetized. The *contents* of the tool list vary by
  cell (11 vs 25 vs 70 entries) — it reflects what the prompt exposed, not the
  harness roster.
- **T7** — three `WebSearch` calls, 11.7–14.0 s each (pass bar 30 s), answered
  by the Codex alpha backend with `origin="grok-4.5"` and 12–40 links; the
  reported URL appears verbatim in a result block. Per phase 5 this measures
  the user-visible search experience inside a Grok session, not a
  Grok-executed search.
- **T8** — correct file, line, and code (`log-f.txt`, `08773f4b`), no
  overflow or truncation error; `Read`'s token cap truncated four reads and
  the model resumed by offset rather than assuming coverage. Downgraded to
  SOFT because it delegated four of the eight files to `Agent` subagents that
  ran on `claude-haiku-4-5` — the probe therefore measures Grok on about half
  the corpus. Round 1's cell read all files itself and passed clean.

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
the only verdict spread (T6) does not order by effort, and the one T6
"medium PASS" depended on reading the suite's own answer key. This is a
different regime from the phase-5 word problem, where reasoning volume and
latency scaled cleanly with effort: these tasks are agentic and easy, so
they do not separate the tiers. Any per-tier guidance needs harder tasks.

### Failure catalogue

- **Whitespace-fidelity `Edit` failure (4.20, T4).** `old_string` prefixed
  with a literal tab against a 4-space file → `String to replace not found`.
  Recovered via `cat -e` after `cat -A` failed (GNU flag, unsupported on
  macOS). The failure mode T1 was designed to catch showed up in T4 instead.
- **Path mangling with perseveration (4.3, T1 and T4).** Rewrote the temp
  path segment `/T/` as `-T`, then re-issued the corrupted path 5 times
  (2 byte-identical) — in T1 even after a `find` result block printed the
  correct path — before running `pwd` and recovering. Costs ~15 s per
  occurrence. Under the ≥4-identical-retry loop bar, but the same wrong
  hypothesis survived contradicting evidence.
- **Fabricated file contents (4.20, T6 — reproduced in both rounds).** After
  confirming `config.yaml` does not exist, it wrote an invented file
  (`timeout: 60` + `other: value`), edited 60 → 30, read it back, and
  reported "**Change completed and verified** … The file is now updated as
  requested". Round 1 invented a `service:` block instead. This is the
  rubric's explicit fail signal, dressed as a workflow.
- **Unbounded search radius (4.5 low/medium, T6).** A `find /Users/yoav`
  whole-home scan (24–26 s of the cell's wall time) for a file the first `ls`
  had already shown absent. One cell went further and called
  `mcp__claude_ai_Google_Drive__search_files` and
  `mcp__claude_ai_Gmail__search_threads {"query":"config.yaml timeout"}`
  against the user's real connected accounts. Sessions were run with
  `--permission-mode bypassPermissions`, so nothing gated it.
- **`count_tokens` 404s.** `POST /v1/messages/count_tokens` returns the
  router's `token counting is not available for routed GPT models` 404; only
  T8 triggers it (8 times), and it did not disturb the run.

### Suite methodology: the answer key must be off the filesystem

Round 1 put each cell's `ground-truth.json` beside its scratch dir; two T6
cells read it (and one read a sibling cell's leftover `config.yaml`) before
answering. Round 2 moved fixtures into isolated `mktemp -d` parents and the
ground truth out of the tree — and cells *still* found the suite by scanning
`/Users/yoav`. **Filesystem distance is not isolation for an agent with
`bypassPermissions`**; a future run needs the answer key on a different
machine or behind a deny rule, and MCP tools disabled for the cells.

### Operational notes

- **No quarantine at all** across 48 runs with ~10 s of pacing between cells.
  Phase 5's `auth_not_found` came from back-to-back requests; a short gap is
  enough to avoid it.
- Every routed request returned 200 (plus the known `count_tokens` 404s).
- `claude --settings <file>` with its own `env` block remains mandatory (see
  phase 5); `ANTHROPIC_CUSTOM_MODEL_OPTION` must name the routing ID under
  test, so each model needs its own settings file.

## Grok-native WebSearch (2026-08-04, CLIProxyAPI 7.2.110, model-router 0.1.11)

Grok-origin `WebSearch` sub-calls now run on xAI's hosted `web_search` tool
instead of Codex's `alpha/search`. Measured on a sandbox child generated from
the router's own `upstream_config_yaml` (isolated dirs, copied auth, live
gateway untouched).

### Two earlier evidence sets are compromised — do not re-derive from them

- **`inject-x-search: true`.** The `sol-search` probe child ran with that flag
  hand-added to its config. Its `response.created` therefore advertised both
  `web_search` and `x_search`, so `tool_choice: "required"` there only forced
  *some* hosted tool — a model picking `x_search` emits a `custom_tool_call`
  and no `web_search_call`. Every tool-forcing and tool-set conclusion from
  that environment was re-established on a clean child before use. The router
  never emits an `xai:` section, so a shipped install cannot be in that state.
- **The `*-raw.sse` files are recorder logs, not wire captures**: no blank-line
  event framing at all (the recorder wrote selected lines), so they cannot
  stand in for a stream. The unit-test fixture is a fresh byte-faithful
  capture (`curl --no-buffer`).

### The wire shape (clean child, verified)

`POST /v1/responses` with `{"model":"grok-4.5","input":<query>,"tools":
[{"type":"web_search"}],"tool_choice":"required","stream":true,
"stream_tool_calls":true,"store":false,"temperature":0.1,"top_p":0.95,
"max_output_tokens":8192}`.

- `response.created` advertises the hosted-tool set **exactly**
  `[{"type":"web_search"}]` — no `x_search`. `tool_choice`, `temperature`,
  `top_p`, `store` and `max_output_tokens` are all accepted and echoed.
- `tools[0].filters.allowed_domains` works (5/5 harvested URLs on the
  requested domain). Excluded/blocked domains were never accepted upstream and
  are not sent; the router filters harvested URLs by host itself.
- Sources arrive on `response.output_item.done` where `item.type ==
  "web_search_call"`, in `item.action.sources[]` as `{"type":"url","url":…}` —
  **no titles**, so links render with the URL as their label.
- The **streamed** shape emits exactly one `web_search_call` item. The 7–9
  items in the phase-6 evidence are a non-streaming artifact, and their later
  source-bearing items repeat the first item's URL set exactly (0 unique URLs
  added), so waiting past the first item buys nothing.

### Closing the stream early quarantines the xAI auth (the design constraint)

| action | next Grok request |
|---|---|
| read the stream to the end (10.6s) | 200 |
| three trivial completions back to back | 200, 200, 200 |
| **harvest at 4.9s, then close the connection** | **503 `auth_unavailable: no auth available (providers=xai)`**, 0 ms, no upstream call |
| probes after that abandon | 503 at +0s, 503 at +30s, 200 at +60s (a stacked burst stayed down ~4 min) |
| probe issued *while* a stream is still being drained | 200 |

So a client disconnect — not request volume — is what takes the xAI auth
offline, for 30–60s. Abandoning after the harvest would therefore have made
every Grok search break the user's next Grok turn. The router instead answers
the sub-call at the harvest and lets a task that owns the stream read it to the
end, which is measured not to block concurrent Grok traffic.

Because *any* early close has this effect, nothing in the router is allowed to
drop a live search stream: the request deadline stops the handler waiting
without cancelling the read, every stream is owned by a registry, and shutdown
stops admitting searches at the signal (one arriving mid-drain fails visibly
rather than opening a stream nothing will finish) and then waits for the
in-flight ones before the managed child is torn down. How many searches may run
at once is deliberately not capped: a parallel sweep should meet the limits of
the user's own xAI subscription — surfaced as `too_many_requests` when it does
— rather than an invented local one. Verified live: with a search answered at 3.2s, `SIGTERM` immediately
after took **6.8s** to exit and the child logged the search as
`200 | 9.999s` — the stream ended by itself. Two searches back to back, each
followed immediately by another Grok request, produced no `auth_unavailable`
at all. Phase
4's `auth_not_found` note and the "early close is clean" reading of the
`sol-search` abort probe are both superseded: the child *process* survived,
but its auth entry did not.

### Failed searches are visible (binary-verified, Claude Code 2.1.222)

The harness renders a `web_search_tool_result` whose `content` is not an array
as `` `Web search error: ${a.content.error_code}` ``, logged at error level and
pushed into what the model reads. A search that cannot run is therefore
reported with `{"type":"web_search_tool_result_error","error_code":
"unavailable"}` plus a one-line detail — no cross-vendor fallback. xAI's own
`grok-build` client emits the same shape on failure, and likewise does not
count a failed search toward `web_search_requests`; the router follows suit,
so a failure does not spend the session's ~200-call WebSearch budget.

### Live end-to-end (sandbox router + real xAI)

| arm | result |
|---|---|
| Grok agent's search correlated to a Claude-branch sub-call (the real topology) | 10 links / 3.2s, 15 links / 8.9s |
| sub-call carried by a Grok route with no correlation | 10 links / 4.2s, 10 links / 3.1s |
| GPT origin, same config | unchanged: `alpha/search`, 33 titled links |
| routed upstream configured but unreachable | `error_code: unavailable`, detail rendered, nothing sent to Anthropic, no search counted — in both streaming and non-streaming framings |

One correlated run out of three came back with no `web_search_call` at all
despite `tool_choice: "required"`, and was reported as a failed search. It
overlapped an in-flight Grok turn; whether concurrency is the cause is not
established. Occasional spurious failures are the known cost of the strict
rule — worth rechecking if users report them.

### Driving Claude Code at a non-default gateway no longer works (2.1.222)

The phase-5 recipe is dead: `--settings` with its own `env` block, a
project-level `settings.local.json`, `CLAUDE_CONFIG_DIR`, and an explicit
`ANTHROPIC_BASE_URL` in the environment were **all** ignored — every headless
run went to the user-level settings' gateway. `CLAUDE_CONFIG_DIR` does move
credential lookup (an isolated dir reports "Not logged in"), so it is read for
auth but not for the base URL. Harness-level sandboxing needs a new approach;
the rendering question above was settled from the bundle instead.

## Grok 4.6 + CLIProxyAPI 7.2.132 pin bump (2026-08-14, model-router 0.1.14)

xAI released grok-4.6 on 2026-08-12 (500K window, effort low/medium/high/
xhigh, image input, a real model card). CLIProxyAPI's embedded registry
gained the ID in v7.2.131; our 7.2.110 pin predates it, so shipping the
route required a pin bump to v7.2.132 (latest at the time). All four
vendored archive sha256s were computed from downloaded artifacts AND
cross-checked against the release's official `checksums.txt` — exact match
on every platform, closing the only-host-platform-is-exercised gap.

Pin-bump risk audit. Full local tree diff between the tags: **423 files,
215 production Go** (an earlier GitHub-Compare-based count of 300 was that
API's file-list cap, not the real total). Scope of what was actually
audited, and how:

- **Full-file diffs read**: codex identity (`codex_executor_request.go`
  Originator pass-through + `codex-tui` default semantically identical —
  the de8ed8a pin rationale holds), the four xAI executor files
  (auth-error normalization only: 403 bad-credentials remapped to 401 for
  refresh-retry; overflow error bodies pass through untouched), the
  thinking mapper (`internal/thinking/apply.go` — per-model level
  clamping from registry-declared levels: 4.6 declares `xhigh`, so
  `xhigh` stays and `max` → `xhigh` there, while 4.5 still clamps both to
  `high`; `routing.rs`'s comment now says so).
- **Skimmed at diffstat/area level**: `sdk/cliproxy/auth` (multi-credential
  selection, cooldown, session affinity, Home-OAuth 401 recovery — for a
  one-credential-per-provider install every request selects the sole
  credential, a path each live run below exercises), `sdk/api/handlers`,
  and the translators. Changes gated behind new config options we don't
  set (codex alpha-search API keys, `support-prompt-cache-key`, Kimi
  thinking-replay cache) are inert here.
- **Not audited**: the child's Claude/Gemini/Antigravity native paths
  (model-router's Claude branch goes straight to Anthropic and never
  rides the child) and the remaining ~190 production files. Coverage for
  what we ship rests on the live exercise below of every serving path we
  use (claude-protocol ingress translation, codex executor, xAI executor,
  streaming and non-streaming, tool use, error translation, search), plus
  the standing containment: `routed-models` doctor check, service-refresh
  prefetch, and byte-deterministic rollback by reverting the commit.
  **The OpenAI-compat inference path is unverified under 7.2.132**: no
  provider is configured on this install (no key to test with), and
  `verify-providers` only checks the provider's own `/models` catalog —
  it never sends inference through the child, so it would NOT catch an
  executor/translator regression there. First signal for installs with
  `[[openai-providers]]` routes would be a failing user turn; rollback is
  the remedy.

Live verification, sandboxed instance (worktree debug build v0.1.14,
scratch XDG dirs under the job tmp, gateway :8790, child :8318, auth-file
copies deleted after; the live 0.1.13/7.2.110 service was never touched and
answered normally afterward):

| check | result |
|---|---|
| doctor | all green; `routed-models`: every routed model served (child catalog has grok-4.6 and grok-4.5); `context-windows`: both Grok routes clipped 500000→258400 |
| grok-4.6 smoke | answers as `grok-4.6-build` (the `-build` served-name convention carries over from 4.5) |
| grok-4.5 smoke | still answers (`grok-4.5-build`) |
| gpt-5.6-sol smoke + tool use | `ok`; clean `tool_use` block for a client tool; thinking block streams at high effort; `end_turn` |
| grok-4.6 effort | `output_config.effort: xhigh` end-to-end success (suffix channel; per-model mapping verified in child source, not on the wire) |
| grok-4.6 overflow | 540087-token prompt → canonical `prompt is too long: 540087 tokens > 500000 maximum` — the xAI overflow phrase and dialect-exact translation hold for 4.6 |
| Grok-origin WebSearch | main turn on grok-4.6 emitted `WebSearch` tool_use; correlated sub-call answered from the xAI backend in 4.1s with real links (bun.sh results). The 2026-08-13 review-agent claim that Grok-native search feeds gzip to the harvester did **not** reproduce on 7.2.132 |

Methodology trap re-confirmed the cheap way: a sub-call whose
`metadata.user_id` is not the JSON-object string carrying `session_id`
never correlates — it silently passes through to Anthropic and returns an
auth error under a sandbox with no API key. First attempt failed exactly
so; fixing the metadata shape made correlation immediate. (Driving Claude
Code itself at the sandbox is still not possible per the 2.1.222 finding
above; direct-curl replication of the two-request shape is the method.)

Route decision: grok-4.6 ships alongside grok-4.5 (flagship-first in
`GROK_MODELS`) rather than replacing it — existing per-user `grok-4.5(*)`
agents keep resolving; docs now recommend 4.6.

## Subagent skill-suppression sentence (2026-08-18, Claude Code 2.1.235, router 0.1.15)

Context: GPT subagents follow skill trigger wording literally; the bundled
`claude-api` skill triggers on any mention of Claude/Anthropic and its payload
killed 17 subagents in one week ("Prompt is too long"), which 0.1.19 patched
around by having setup turn the skill off globally. The general fix tested
here: extend the subagent identity sentence to also forbid proactive skill
invocation, so main agents decide skill use and relay it.

Method: A/B against a second router instance on :8790 built from the patched
tree (`[upstreams.cliproxy] mode = "external"` pointed at the production
managed CLIProxyAPI on :8317, own `XDG_STATE_HOME` for the serve lock). The
`claude-api` skillOverride was removed for the test. Drivers: headless sonnet
sessions in a scratch dir, each instructed to spawn exactly one
`model-router:gpt-5.6-sol(high)` subagent. Tasks: T1 = summarize a Claude Code
SessionStart hook script (Claude-adjacent, the observed death mode); T2 =
write a TypeScript snippet calling the Claude API via `@anthropic-ai/sdk`
(squarely inside the skill's trigger). Outcome read from the driver's
transcript jsonl: any sidechain `Skill` tool_use with `claude-api`.

| arm | T1 invoked | T2 invoked |
|---|---|---|
| baseline (0.1.13 sentence, no skill clause) | 3/3 | 3/3 |
| v1: "use a skill only when your task names it" | 0/3 | 3/3 |
| v2: "Never invoke a skill on your own initiative … only when your task explicitly instructs you to use that skill" | 0/1 | 0/3 |

- v1's "names it" failed on T2 because the task text "the Claude API" reads
  as naming the `claude-api` skill; v2's explicit-instruction wording closed
  that.
- No trial hit "Prompt is too long" at a fresh subagent's context; the
  historical deaths came from fuller contexts. Invocation rate is the metric.
- v2 T2 subagents produced correct SDK streaming code without the skill, so
  suppression did not degrade the deliverable.
- Contradicting the 2.1.222 finding above: on 2.1.235 a `--settings` file
  with its own `env` block **does** override the user-level
  `ANTHROPIC_BASE_URL` for headless runs (verified by echoing the env and by
  the request landing in the :8790 instance's capture file). Harness-level
  sandboxing by settings file works again.
- Capture records inbound request bodies (pre-injection), so the injected
  identity text never appears in `capture.jsonl`; `cc_is_subagent=true` in
  the captured attribution block is the marker that the subagent path was
  exercised.
