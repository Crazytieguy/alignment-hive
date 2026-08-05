---
name: setup
description: Set up, verify, repair, or uninstall the model-router Claude/GPT routing gateway — binary install, CLIProxyAPI, Codex OAuth, optional Grok (xAI) family, OS service, and Claude Code settings wiring.
---

# model-router setup

`ROUTER="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"` — every command below
goes through it (it downloads the pinned router binary on first use). This
flow is idempotent; `$ROUTER doctor` at any point shows what's left to do.
macOS and Linux only.

## Install flow

1. **Diagnose**: `$ROUTER doctor`. If everything is already green, skip to
   step 5.
2. **Upstream binary**: `$ROUTER ensure-upstream` — downloads the pinned,
   checksum-verified CLIProxyAPI release into the cache. No-op when present.
3. **Codex auth**: `$ROUTER doctor` reports auth state. If it found and
   imported an existing CLIProxyAPI Codex login, say so and move on. If not,
   the user must run the interactive login themselves (it opens a browser
   for Codex OAuth), either way works: paste the full expanded
   `.../scripts/bootstrap.sh login` command for them to run in a separate
   terminal, or have them type `! $ROUTER login` (expanded to the real path)
   in the prompt. Verify with `$ROUTER doctor` afterwards.
4. **Service**: `$ROUTER service install` (installs and starts the
   launchd/systemd user service), then `$ROUTER doctor` until healthy.
   The defaults need no config file — all three GPT routes (sol, terra,
   luna), port 8787. Only if 8787 is taken, write `port = <other>` to
   `~/.config/model-router/config.toml` (`$ROUTER config-template` prints
   the annotated template) and `$ROUTER service restart`.
5. **Wire Claude Code (ask first)**: find where the plugin is installed by
   checking which settings file lists it — `~/.claude/settings.json`
   (global) or the project's `.claude/settings.json` /
   `.claude/settings.local.json` — and add to that same file's `env` block,
   using `.base_url` from `$ROUTER doctor --json` (it embeds a per-install
   ingress token, so requests from other local processes are rejected):
   ```json
   "ANTHROPIC_BASE_URL": "<base_url from doctor --json>",
   "_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL": "1",
   "ENABLE_TOOL_SEARCH": "true",
   "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "258400",
   "ANTHROPIC_CUSTOM_MODEL_OPTION": "gpt-5.6-sol",
   "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "GPT-5.6 Sol"
   ```
   `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` keeps Claude models' native 1M
   context windows. Claude Code grants those only when the base URL is
   `api.anthropic.com`, so behind the gateway Fable 5, Opus 5 and Sonnet 5
   silently fall back to 200K wherever the model string carries no `[1m]`
   suffix — including agent definitions the user did not write. The flag is
   undocumented (Claude Code names it in its own copy, for proxies that front
   the real API — which is what the Claude branch is); it also restores the
   rest of Claude Code's first-party behaviour, including error reporting,
   refusal fallback and first-party billing headers, and it disables
   `/v1/models` gateway discovery. GPT routing is unaffected: the router
   strips `anthropic-beta` and credentials on that branch, and the GPT window
   still comes from `CLAUDE_CODE_MAX_CONTEXT_TOKENS`.
   `ENABLE_TOOL_SEARCH` matters: tool search silently disables itself behind
   a gateway. `CLAUDE_CODE_MAX_CONTEXT_TOKENS` declares the GPT models'
   context window — it only applies to model IDs that don't start with
   `claude-` (the `gpt-5.6-*` routes), so Claude models keep their
   built-in windows; 258400 is the Codex backend's effective input limit.
   On an existing install with configured open-weights routes, run
   `$ROUTER doctor` after raising the value — it fails with the fix
   spelled out if a route's window no longer fits under the new
   declaration. The custom-model pair adds one
   "GPT-5.6 Sol" entry to the /model picker, so main-model GPT use gets
   the declared context window too; terra and luna stay subagent-only.
6. Tell the user to restart Claude Code sessions (env is read at startup),
   and that the GPT agents and `choosing-models` skill are now available.
   Offer to run smoke tests. A fresh `claude -p` reads the step 5 settings
   at its own startup, so they work without restarting the current session.
   Don't env-prefix the step 5 variables onto them: once the settings env
   block exists it silently overrides shell-provided values. (If the user
   declined step 5, prefixing `ANTHROPIC_BASE_URL=<base_url>` onto the
   routing test is instead required, and the window check is meaningless —
   against the direct Anthropic API it prints 1000000 regardless of the
   flag.)
   Routing: `claude -p 'reply with ok' --model gpt-5.6-sol`.
   1M windows, to confirm they survive the current Claude Code version:
   `claude -p 'say ok' --model claude-fable-5 --output-format json | jq
   '.modelUsage[].contextWindow'` — it must print 1000000. On 200000,
   re-check the step 5 wiring first (right settings file; run from inside
   the project if the wiring is project-scoped); only if it is correct did
   a Claude Code release drop the flag.
7. Ask whether the user also wants (a) open-weights models (Kimi, GLM, ...)
   served through an OpenAI-compatible host they have an API key for — if
   yes, read `references/open-weights.md` and follow it; (b) Grok models
   under their own xAI subscription login (no API key) — if yes, read
   `references/grok.md` and follow it; (c) agents for other model x effort
   combinations — if yes, follow `references/custom-agents.md`.

## Repair

`$ROUTER doctor` names the failing layer (config, binary cache, auth,
service, upstream). Fix only that layer using the matching step above;
`$ROUTER service restart` after config changes. Doctor does not see Claude
Code's env block: if Claude models report a 200K window, re-check step 5's
`_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` with the step 6 window check —
a Claude Code release could drop the flag.

## Disable / uninstall

1. Remove `ANTHROPIC_BASE_URL` (and optionally the other keys step 5 added)
   from the settings file it was written to — this alone restores direct
   Anthropic access.
2. `$ROUTER service uninstall`.
3. Optionally delete `~/.config/model-router`, `~/.local/state/model-router`,
   and `~/.cache/model-router` (the state dir includes the Codex auth login —
   warn before deleting).
