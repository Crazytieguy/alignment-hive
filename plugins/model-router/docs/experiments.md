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
  upstream limit.
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
  open-weights host (no provider key available). **Retest trigger:** any Claude
  Code upgrade — this is undocumented, reverse-engineered behavior, and a
  version that starts sizing context differently would turn scaling into
  silent overruns.

Externally sourced catalog facts (not measured here, 2026-07-27): GLM-5.2
advertises a 1M-token window under the separate `glm-5.2[1m]` model ID
(base ID serves less); Kimi K3 is listed at 1,048,576 on OpenRouter. Host-
served windows can be lower than the vendor's advertised number — the Codex
backend serving 272K of GPT-5.6's advertised 1.05M is the same pattern — and
OpenRouter's `context_length` is the maximum across its sub-providers.

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
the non-streaming path). **Retest triggers:** any Claude Code upgrade (the
classifier strings and recovery behavior are reverse-engineered), any
CLIProxyAPI upgrade (its error translation may change shape — if it starts
preserving `context_length_exceeded`, detection can tighten to the code),
and Codex backend window changes (272K/95% → update `GPT_CONTEXT_WINDOW`).

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
