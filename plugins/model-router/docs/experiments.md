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

## Still open
- Long sessions and heavy tool loops against real Codex; more ToolSearch
  samples (single positive so far).
- Gateway model discovery in interactive sessions (cosmetic: /model picker
  entries; not needed for the agent-file or Workflow flows).
- Compliance: user reviewed the subscription-OAuth consideration and accepted
  the risk (2026-07-20); revisit before any public release of the plugin.
- The Workflow `opts.model` string-typing is an implementation detail, not a
  documented contract — retest on Claude Code upgrades.
