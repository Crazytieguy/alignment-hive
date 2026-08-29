---
name: setup
description: Set up, verify, repair, or uninstall the model-router Claude/GPT routing gateway — binary install, CLIProxyAPI, Codex OAuth, optional Grok (xAI) family, OS service, and Claude Code settings wiring.
---

# model-router setup

`ROUTER="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"` — every command below
goes through it (it resolves the pinned router binary on first use). This
flow is idempotent; `$ROUTER doctor` at any point shows what's left to do.
macOS and Linux only.

## Wrong platform (check this first)

`$ROUTER platform-check` — silent and exit 0 means all is well, go on to the
install flow. It only fails when a platform-specific plugin entry for a
*different* machine is enabled: those bundle one target's binary, so the wrong
one cannot run. It prints the entry that should be there, e.g.
`model-router-aarch64-apple-darwin`.

Fix it with the claude CLI — editing `enabledPlugins` by hand installs
nothing (platform entries are archive-sourced and never load without a real
install):

1. Read all settings files (`~/.claude/settings.json`,
   `~/.claude/settings.local.json`, `.claude/settings.json`,
   `.claude/settings.local.json`) and note every enabled model-router key,
   plain or platform-specific.
2. Run `claude plugin install <printed-name>@alignment-hive` with
   `--scope user` if any of those keys is in `~/.claude/settings.json` or
   `~/.claude/settings.local.json`, else `--scope local`. Never
   `--scope project`: a checked-in platform key breaks teammates on other
   platforms.
3. Verify: exit 0 and the entry appears in `claude plugin list`. On failure,
   remove nothing — report and stop.
4. Remove every other model-router key from every settings file, so the
   entry the install just wrote is the only one enabled — the plain
   `model-router@alignment-hive` and any platform entry define the same
   commands, skills and agents, so two of them load two copies.
   `~/.claude/settings.local.json` is never read by the plugin loader;
   remove any model-router key there too. If a key sits in a checked-in
   `.claude/settings.json`, warn first: removing it takes the plugin away
   from collaborators, who will each need to install the entry for their
   own machines.

Then tell the user to restart Claude Code and re-run this skill. Nothing else
here works until the binary resolves.

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
   "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "258400"
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
   declaration.
   Then list the routes in the `/model` picker: one row per routing ID that
   `$ROUTER doctor` lists under `routed-models` (the three GPT routes on a
   default install, plus any Grok or open-weights routes already
   configured). Claude Code reads `modelPicker` only from
   `~/.claude/settings.json` (project and local files are ignored) and only
   from 2.1.242 on; add it there as a sibling of `env`, and drop the
   `ANTHROPIC_CUSTOM_MODEL_OPTION` pair an earlier install wrote (the rows
   replace it):
   ```json
   "modelPicker": {
     "options": [
       { "model": "gpt-5.6-sol", "label": "GPT-5.6 Sol" },
       { "model": "gpt-5.6-terra", "label": "GPT-5.6 Terra" },
       { "model": "gpt-5.6-luna", "label": "GPT-5.6 Luna" }
     ]
   }
   ```
   The rows follow the built-in Claude models, and a routed ID picked there
   gets the declared context window like any other. Rows are never checked
   against the router — an unserved row is selectable and fails on its
   first turn — so drop a row when its route goes. Two cases keep the
   single-slot pair instead, in the wired file's `env` block (one entry
   only; the other routes stay off the picker, reachable through agents or
   `--model`): project-scoped wiring, where user-level rows would show in
   every project, including ones that don't go through the gateway; and
   Claude Code below 2.1.242 (`claude --version`), where the key is
   unmeasured.
   ```json
   "ANTHROPIC_CUSTOM_MODEL_OPTION": "gpt-5.6-sol",
   "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "GPT-5.6 Sol"
   ```
6. Tell the user to restart Claude Code sessions (settings are read at
   startup), and that the GPT agents and `choosing-models` skill are now
   available.
   Offer to run smoke tests. A fresh `claude -p` reads the step 5 settings
   at its own startup, so they work without restarting the current session.
   Don't env-prefix the step 5 variables onto them: once the settings env
   block exists it silently overrides shell-provided values. (If the user
   declined step 5, prefixing `ANTHROPIC_BASE_URL=<base_url>` onto the
   routing test is instead required, and the window check is meaningless —
   against the direct Anthropic API it prints 1000000 regardless of the
   flag.)
   Routing: `claude -p 'reply with ok' --model gpt-5.6-sol`.
   Picker rows: `/model` in a fresh interactive session lists them after
   the Claude models.
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
a Claude Code release could drop the flag. A picker row that fails on its
first turn with "There's an issue with the selected model" names a route the
gateway doesn't serve: compare the rows with doctor's `routed-models`. A
`fallback-model` failure means a Claude Code `fallbackModel` setting is in
effect, which silently re-runs a failed GPT/Grok subagent on a Claude model;
the fix is removing that setting (then restarting Claude Code), and it is
the user's call.

## Disable / uninstall

1. Remove `ANTHROPIC_BASE_URL` (and optionally the other keys step 5 added,
   `modelPicker` included) from the settings file it was written to — this
   alone restores direct Anthropic access. Also remove a `model` key naming
   a routed ID (written when a picker row was saved as the default), or new
   sessions start on a model Anthropic doesn't serve.
2. `$ROUTER service uninstall`.
3. Optionally delete `~/.config/model-router`, `~/.local/state/model-router`,
   and `~/.cache/model-router` (the state dir includes the Codex auth login —
   warn before deleting).
