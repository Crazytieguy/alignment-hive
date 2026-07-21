---
name: setup
description: Set up, verify, repair, or uninstall the model-router Claude/GPT routing gateway — binary install, CLIProxyAPI, Codex OAuth, OS service, and Claude Code settings wiring.
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
   "ENABLE_TOOL_SEARCH": "true"
   ```
   `ENABLE_TOOL_SEARCH` matters: tool search silently disables itself behind
   a gateway.
6. Tell the user to restart Claude Code sessions (env is read at startup),
   and that the GPT agents and `choosing-models` skill are now available.
   Offer to run a smoke test that works without a restart:
   `ANTHROPIC_BASE_URL=<base_url> claude -p 'reply with ok' --model
   claude-gpt-5.6-sol`.

## Repair

`$ROUTER doctor` names the failing layer (config, binary cache, auth,
service, upstream). Fix only that layer using the matching step above;
`$ROUTER service restart` after config changes.

## Disable / uninstall

1. Remove `ANTHROPIC_BASE_URL` (and optionally `ENABLE_TOOL_SEARCH`) from
   the settings file it was written to — this alone restores direct
   Anthropic access.
2. `$ROUTER service uninstall`.
3. Optionally delete `~/.config/model-router`, `~/.local/state/model-router`,
   and `~/.cache/model-router` (the state dir includes the Codex auth login —
   warn before deleting).
