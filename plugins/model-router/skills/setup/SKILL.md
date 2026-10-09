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
   The defaults need no config file — every built-in GPT route, port
   8787. Only if 8787 is taken, write `port = <other>` to
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
   "CLAUDE_CODE_GATEWAY_HINT_HEADERS": "1",
   "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "1000000"
   ```
   and, as a sibling of `env`, a `modelSettings` entry for each route:
   apply every fix `$ROUTER doctor`'s `context-windows` line prints, e.g.:
   ```json
   "modelSettings": {
     "gpt-6-astra": { "autoCompactWindow": 258400 },
     "gpt-6.1-sol": { "autoCompactWindow": 258400 },
     "gpt-6-luna": { "autoCompactWindow": 258400 }
   }
   ```
   `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` keeps Claude sessions on
   Claude Code's first-party behaviour (refusal fallback, WebSearch modes,
   API betas), which it otherwise withholds behind a custom base URL.
   Tool search disables itself behind a gateway unless
   `ENABLE_TOOL_SEARCH` is set. `CLAUDE_CODE_GATEWAY_HINT_HEADERS` lets the
   router recognize compaction requests. `CLAUDE_CODE_MAX_CONTEXT_TOKENS` is
   every route's context window; each route compacts at its own
   `modelSettings` entry. For the GPT routes 258400 is the recommended
   limit (input past 272K is billed at a higher rate; they accept up to
   828400). Per-model entries need Claude Code 2.1.288+ (`claude
   --version`; have the user update first if older).
   Then list the routes in the `/model` picker: one row per shipped GPT
   route below, plus one per Grok or open-weights route already configured
   (the `routed-models` line of `$ROUTER doctor`; older GPT routes stay
   served but off the picker, so an earlier install's GPT-6 Sol or GPT-5.6
   rows are dropped). Claude Code reads `modelPicker` only from
   `~/.claude/settings.json` (project and local files are ignored) and only
   from 2.1.242 on; add it there as a sibling of `env`, and drop the
   `ANTHROPIC_CUSTOM_MODEL_OPTION` pair an earlier install wrote (the rows
   replace it):
   ```json
   "modelPicker": {
     "options": [
       { "model": "gpt-6-astra", "label": "GPT-6 Astra" },
       { "model": "gpt-6.1-sol", "label": "GPT-6.1 Sol" },
       { "model": "gpt-6-luna", "label": "GPT-6 Luna" }
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
   "ANTHROPIC_CUSTOM_MODEL_OPTION": "gpt-6-astra",
   "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "GPT-6 Astra"
   ```
6. Tell the user to restart Claude Code sessions (settings are read at
   startup), and that the GPT agents and `delegating-to-models` skill are now
   available.
   Offer to run smoke tests. A fresh `claude -p` reads the step 5 settings
   at its own startup, so they work without restarting the current session.
   Don't env-prefix the step 5 variables onto them: once the settings env
   block exists it silently overrides shell-provided values. (If the user
   declined step 5, prefixing `ANTHROPIC_BASE_URL=<base_url>` onto the
   routing test is instead required.)
   Routing: `claude -p 'reply with ok' --model gpt-6-astra`.
   Picker rows: `/model` in a fresh interactive session lists them after
   the Claude models.
7. Ask whether the user also wants (a) open-weights models (Kimi, GLM, ...)
   served through an OpenAI-compatible host they have an API key for — if
   yes, read `references/open-weights.md` and follow it; (b) Grok models
   under their own xAI subscription login (no API key) — if yes, read
   `references/grok.md` and follow it. Agents from an earlier setup named
   `<routing-id>(<effort>)` (in `~/.claude/agents/` or the project's
   `.claude/agents/`): offer to replace them following
   `references/custom-agents.md`.

## Repair

`$ROUTER doctor` names the failing layer (config, binary cache, auth,
service, upstream). Fix only that layer using the matching step above;
`$ROUTER service restart` after config changes. A picker row that fails on its
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
