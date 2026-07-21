# model-router

> **Experimental.** This routes all your Claude Code traffic through a local
> gateway; a bug here breaks your sessions until you remove one settings line
> (`ANTHROPIC_BASE_URL`), which always restores direct Anthropic access.

model-router lets Claude Code delegate to GPT models as native subagents,
alongside Claude models, in the same session. It runs a small loopback
Anthropic-format gateway on your machine:

- Requests for Claude models pass through **byte-exact** to
  `api.anthropic.com`, with your existing claude.ai OAuth login and
  subscription billing untouched.
- Requests for an explicit allowlist of GPT routing IDs (e.g.
  `claude-gpt-5.6-sol`) are stripped of all Anthropic credentials and
  forwarded to a supervised local [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)
  instance, which translates them to the Codex protocol under your separate
  Codex subscription OAuth. No API keys required on either side.

Routed GPT models receive an honest-identity system block, so they never
present themselves as Claude.

## What you get

- GPT subagents — `gpt-5.6-sol(medium)`, `gpt-5.6-sol(high)`,
  `gpt-5.6-terra(high)`, `gpt-5.6-luna(high)` — usable from any project,
  plus dynamic per-invocation model + effort choice in Workflow
  orchestration.
- Optional open-weights subagents — add Kimi, GLM, or other models from any
  OpenAI-compatible host you have an API key for during `/model-router:setup`.
- A `choosing-models` skill that helps Claude pick the right model and
  effort when delegating.
- An OS service (launchd/systemd user unit) that keeps the router alive.
  macOS and Linux only.

## Setup

Run `/model-router:setup` and follow along; it also covers repair and
uninstall. `model-router doctor` diagnoses the whole stack in one shot.

## Compliance

Both sides ride supported paths. The Claude side is a standard
`ANTHROPIC_BASE_URL` gateway configuration: requests are forwarded
byte-exact and billed to your own subscription, with nothing impersonated.
The GPT side spends your own Codex subscription through its own OAuth
login — an arrangement OpenAI has publicly blessed regardless of the harness
driving it: Codex lead Thibault Sottiaux
[posted the CLIProxyAPI-into-Claude-Code recipe himself](https://x.com/thsottiaux/status/2076119366647894371)
("we don't care about the harness"), and ChatGPT sign-in is a
[documented Codex auth method](https://developers.openai.com/codex/auth).

## Development

Point `MODEL_ROUTER_DEV` at a locally built binary to bypass the release
download in `scripts/bootstrap.sh`. `docs/experiments.md` records the
measured Claude Code behavior this design rests on.
