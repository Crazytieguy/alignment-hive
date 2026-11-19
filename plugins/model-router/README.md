# model-router

> **Beta.** This routes all your Claude Code traffic through a local
> gateway; a bug here breaks your sessions until you take its wiring out of
> your settings (`ANTHROPIC_BASE_URL`, or `HTTPS_PROXY` and
> `proxyAuthHelper`), which always restores direct Anthropic access.

model-router lets Claude Code delegate to GPT models as native subagents,
alongside Claude models, in the same session. Models from different labs
make different mistakes, so a GPT model reviewing Claude's plan, code and
analysis catches what a second Claude would miss. It supports either a
ChatGPT subscription or an OpenAI API key.

It runs a small loopback Anthropic-format gateway on your machine:

- Requests for Claude models pass through **byte-exact** to
  `api.anthropic.com`, with your existing claude.ai OAuth login and
  subscription billing untouched.
- Requests for an explicit allowlist of GPT routing IDs (e.g.
  `gpt-6.2-astra`) are stripped of all Anthropic credentials and
  forwarded to a supervised local [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)
  instance, which translates them to the Codex protocol under your
  Codex login or API key.

Routed GPT models receive an honest-identity system block, so they never
present themselves as Claude.

## What you get

- GPT subagents — `gpt-6.2-astra`, `gpt-6.2-sol`, `gpt-6.1-terra`,
  `gpt-6-luna` — usable from any project at any reasoning effort, which
  Claude sets per call (Claude Code 2.1.292+), and as models in Workflow
  scripts.
- Adversarial code and plan reviewers on GPT-6.2 Sol, with prompts tuned
  to push back on what Claude wrote.
- A `delegating-to-models` skill that helps Claude pick the model and
  effort when delegating.
- A `-fast` twin of each GPT subagent (e.g. `gpt-6.2-sol-fast`) on
  Codex's fast tier: 1.5–2x faster for 2–2.5x the usage. Claude Code's own
  `/fast` doesn't reach GPT routes.
- On plans with OpenAI's Ultrafast tier, `-ultrafast` twins of GPT-6.2 Sol
  and Astra: up to 4.5x faster for 6x the usage. The session-start notice
  offers them when your plan has them.
- Optional Grok, Muse, Gemini and open-weights models (Kimi, GLM, ...),
  added during `/model-router:setup`. Grok and Muse use your own xAI or
  Muse Code subscription login; Gemini uses a Google AI Studio API key.
  Muse is not a supported path under Meta's terms (see Compliance), and
  setup shows you the terms first.
- OpenAI Daybreak (Trusted Access for Cyber), for accounts OpenAI has
  approved: `/model-router:setup` lets you choose Daybreak (fewer cyber
  refusals on the models your approval covers) or standard safeguards for
  GPT requests, after showing what Daybreak means for your data. The
  session-start notice says when OpenAI lists Daybreak for your account
  and you haven't chosen. To detect this, the router reads your account's
  model catalog from OpenAI's Codex backend (chatgpt.com), as Codex does:
  with the Codex login, or with an API key unless you chose standard
  safeguards.
- Working WebSearch in GPT sessions, answered from the routed model's own
  search backend in a few seconds, with links.
- An experimental auto-mode classifier, off by default (setup asks once).
  In auto mode, once a session has a GPT reply with tool calls, Claude
  Code checks every later tool call with Sonnet on your Claude plan. With
  this on, the gateway checks GPT's tool calls on GPT instead, so the
  session can stay on Anthropic's free check. It saves the most in
  sessions that make many tool calls on a cheap model. It is
  sometimes stricter than Claude Code's own check, denies a call it gets
  no verdict on, and some calls wait several seconds longer.
- An OS service (launchd/systemd user unit) that keeps the router alive.
  macOS and Linux only.

## Setup

Run `/model-router:setup` and follow along; it also covers repair and
uninstall. `model-router doctor` diagnoses the whole stack in one shot.

Setup makes the gateway Claude Code's HTTPS proxy. Claude Code still talks
to `api.anthropic.com`, so Remote Control keeps working and sessions in the
Claude desktop app go through the gateway too. The gateway decrypts only
Claude Code's own `api.anthropic.com` traffic, with a local certificate
authority restricted to that host (on macOS, setup adds it to your login
keychain, which may ask for your password); everything else passes through
undecrypted. While the gateway is down, Claude Code and the programs it
runs have no network, until the service restarts it or you pick Bypass
router in the notice model-router shows (at the next start or `/clear`)
and restart Claude Code. This wiring is untested on Linux and WSL.

Earlier installs wired through `ANTHROPIC_BASE_URL` keep working, though
Remote Control refuses to start under it and desktop-app sessions don't go
through the gateway; `/model-router:setup` moves them to the proxy if you
want. Setup also falls back to the base URL when you keep a proxy or
certificate setting of your own that conflicts with the proxy wiring.

The desktop app's model menu lists only Claude models; type
`/model gpt-6.2-sol` (or another routing ID) in its message box instead.

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
