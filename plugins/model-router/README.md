# model-router

> **Beta.** This routes all your Claude Code traffic through a local
> gateway; a bug here breaks your sessions until you take its wiring out of
> your settings (`ANTHROPIC_BASE_URL`, or `HTTPS_PROXY` and
> `proxyAuthHelper`), which always restores direct Anthropic access.

model-router lets Claude Code delegate to GPT models as native subagents,
alongside Claude models, in the same session. It runs a small loopback
Anthropic-format gateway on your machine:

- Requests for Claude models pass through **byte-exact** to
  `api.anthropic.com`, with your existing claude.ai OAuth login and
  subscription billing untouched.
- Requests for an explicit allowlist of GPT routing IDs (e.g.
  `gpt-6-astra`) are stripped of all Anthropic credentials and
  forwarded to a supervised local [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)
  instance, which translates them to the Codex protocol under your separate
  Codex subscription OAuth. No API keys required on either side, though an
  OpenAI API key (billed per token) works in place of the subscription.

Routed GPT models receive an honest-identity system block, so they never
present themselves as Claude.

## What you get

- GPT subagents — `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6.1-terra`,
  `gpt-6-luna` — usable from any project at any reasoning effort (Claude
  picks it per call; Claude Code 2.1.292+), plus dynamic per-invocation
  model + effort choice in Workflow orchestration.
- A `-fast` twin of each of those routes (e.g. `gpt-6.1-sol-fast`) on
  Codex's fast tier, as an agent, a Workflow model and an optional `/model`
  row: 1.5–2x faster for 2.5x the subscription usage (about twice the
  per-token price on an API key). Claude Code's own `/fast` doesn't reach
  GPT routes.
- Optional open-weights subagents — add Kimi, GLM, or other models from any
  OpenAI-compatible host you have an API key for during `/model-router:setup`.
- Optional Grok family — grok-4.7 under your own xAI subscription OAuth
  login (no API key), riding the same CLIProxyAPI instance. Off by
  default; opt in during `/model-router:setup`.
- Optional Muse family — Meta's Muse Wonder and Muse Wonder Full (Power and
  Maximum plans only) under your
  own Muse Code subscription login (no API key), on the same instance, in
  Standard mode unless you opt in to Contributor mode, which Meta trains
  on (setup presents the choice). Off by default; setup puts
  Meta's terms in front of you first, since they restrict the subscription
  credential to Muse Code, and model-router is outside it.
- An experimental auto-mode classifier, off by default (setup asks once).
  Once a session has a GPT reply with tool calls, Claude Code's auto mode
  checks those calls and every one after them with Sonnet on your Claude
  plan. With this
  on, the gateway checks GPT replies' tool calls on GPT instead, and
  Claude's keep Anthropic's free check. It pays off most for heavy Luna or
  Haiku users, whose sessions make many tool calls for little usage of
  their own. It is sometimes stricter than Claude Code's own check, denies
  a call it gets no verdict on, and some calls wait several seconds longer.
- A `delegating-to-models` skill that helps Claude pick the right model
  and effort when delegating.
- Adversarial code and plan reviewers on GPT-6.1 Sol — a second model with
  prompts tuned to push back on what Claude wrote.
- Working WebSearch on GPT sessions: Claude Code's WebSearch runs a side
  LLM call that loses its links through the Codex translation (and takes
  20–70s). The gateway answers it from the Codex search backend instead —
  structured links in ~1–3s (`[web-search]` in the config to opt out; Grok
  and Muse agents keep their vendor's search even then; only disabling the
  family stops it). The backend follows whichever agent asked: a GPT subagent's search uses the
  Codex backend even under a Claude main session, a Grok agent's search
  uses xAI's, a Muse agent's uses Meta's, and a Claude subagent's search
  stays on Anthropic even under a GPT main session.
- An OS service (launchd/systemd user unit) that keeps the router alive.
  macOS and Linux only.

## Setup

Run `/model-router:setup` and follow along; it also covers repair and
uninstall. `model-router doctor` diagnoses the whole stack in one shot.

Claude Code reaches the gateway one of two ways:

- **As its HTTPS proxy** (recommended). Claude Code still talks to
  `api.anthropic.com`, so Remote Control keeps working and sessions in the
  Claude desktop app route too. The gateway decrypts only Claude Code's own
  `api.anthropic.com` traffic, with a local certificate authority that is
  restricted to that one host (on macOS, setup adds it to your login
  keychain, which asks for your password); every other connection,
  including your tools' own Anthropic calls, passes through untouched.
  While the gateway is down, Claude Code and the tools it runs have no
  network until it restarts or you bypass it. Untested on Linux and WSL.
- **As its `ANTHROPIC_BASE_URL`**, the original wiring. Remote Control
  refuses to start under it, and the desktop app ignores it. Earlier
  installs keep working on it; setup moves them to the proxy when you ask.

## Compliance

Both sides ride supported paths. The Claude side is a standard gateway
configuration, `ANTHROPIC_BASE_URL` or an HTTPS proxy with your own
certificate authority: requests are forwarded byte-exact and billed to your
own subscription, with nothing impersonated.
The GPT side spends your own Codex subscription through its own OAuth
login — an arrangement OpenAI has publicly blessed regardless of the harness
driving it: Codex lead Thibault Sottiaux
[posted the CLIProxyAPI-into-Claude-Code recipe himself](https://x.com/thsottiaux/status/2076119366647894371)
("we don't care about the harness"), and ChatGPT sign-in is a
[documented Codex auth method](https://developers.openai.com/codex/auth).
The optional Muse family is not a supported path: Meta's terms (§3.2)
restrict the subscription credential to Muse Code, and CLIProxyAPI
presents itself as Muse Code. Setup shows the terms before enabling it.

## Development

Point `MODEL_ROUTER_DEV` at a locally built binary to bypass the release
download in `scripts/bootstrap.sh`.
[`experiments/model-router.md`](../../experiments/model-router.md) records
the measured Claude Code behavior this design rests on.
