---
name: setup
description: Set up, verify, repair, or uninstall the model-router Claude/GPT routing gateway — binary install, CLIProxyAPI, Codex OAuth or OpenAI API key, optional Grok (xAI) and Muse (Meta) families, OS service, and Claude Code settings wiring.
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
3. **GPT auth**: `$ROUTER doctor` reports auth state. If it found and
   imported an existing CLIProxyAPI Codex login, say so and move on. If not,
   ask whether the user will sign in with a ChatGPT subscription
   (recommended) or use an OpenAI API key (billed per token). For the
   subscription, the user must
   run the interactive login themselves (it opens a browser
   for Codex OAuth), either way works: paste the full expanded
   `.../scripts/bootstrap.sh login` command for them to run in a separate
   terminal, or have them type `! $ROUTER login` (expanded to the real path)
   in the prompt. For an API key, write `[gpt]` / `auth = "api-key"` to
   `~/.config/model-router/config.toml`, and add a placeholder to
   `~/.config/model-router/secrets.toml` (create it chmod 600 if absent):
   ```toml
   [gpt]
   api-key = "REPLACE-WITH-YOUR-KEY"
   ```
   Ask the user to swap in their real key and wait for them to confirm
   (`$ROUTER service restart` if the service is already installed). Verify
   with `$ROUTER doctor` afterwards.
4. **Service**: `$ROUTER service install` (installs and starts the
   launchd/systemd user service), then `$ROUTER doctor` until healthy.
   The defaults need no config file — every built-in GPT route, port
   8787. Only if 8787 is taken, write `port = <other>` to
   `~/.config/model-router/config.toml` (`$ROUTER config-template` prints
   the annotated template) and `$ROUTER service restart`.
5. **Wire Claude Code (ask first)**: find where the plugin is installed by
   checking which settings file lists it — `~/.claude/settings.json`
   (global) or the project's `.claude/settings.json` /
   `.claude/settings.local.json`; the wiring goes in that same file. There
   are two wirings; offer the HTTPS proxy first:
   - **HTTPS proxy (recommended).** Claude Code keeps talking to
     `api.anthropic.com` through the router as its `HTTPS_PROXY`, so Remote
     Control keeps working and desktop-app sessions route too. The router
     decrypts only Claude Code's own `api.anthropic.com` traffic, with a
     local certificate authority that can vouch for no other host;
     everything else (your tools' traffic included) passes through
     untouched. The trade: while the router is down, Claude Code and the
     tools it runs have no network. On macOS, first `$ROUTER trust-ca`: it
     adds that CA to the login keychain, and macOS asks for the user's
     password — say so before running it. On Linux (and WSL, which runs
     the Linux build) there is no trust step, as the wiring names the CA
     in `NODE_EXTRA_CA_CERTS`; tell the user this path is untested there,
     and that the base-URL wiring is the tested one. Then `$ROUTER settings
     proxy --apply --project-dir <project>` (add `--file <that file>` for a
     file other than the one it reports). It sets `HTTPS_PROXY`,
     `CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER`,
     `CLAUDE_CODE_GATEWAY_HINT_HEADERS` (lets the router recognize
     compaction requests) and a `proxyAuthHelper` that prints the router's
     credential, and removes an earlier base-URL wiring. When it prints
     `"applied": false`, its `reason` says what stands in the way: another
     proxy or a `NO_PROXY` the next session would honour instead, or a
     different `NODE_EXTRA_CA_CERTS`. These are the user's own settings, so
     don't remove them unasked: offer the base-URL wiring instead.
   - **Base URL.** Add to the file's `env` block, using `.base_url` from
     `$ROUTER doctor --json` (it embeds a per-install ingress token, so
     requests from other local processes are rejected):
     ```json
     "ANTHROPIC_BASE_URL": "<base_url from doctor --json>",
     "_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL": "1",
     "CLAUDE_CODE_GATEWAY_HINT_HEADERS": "1"
     ```
     `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` keeps Claude sessions on
     Claude Code's first-party behaviour (refusal fallback, WebSearch
     modes, API betas), which it otherwise withholds behind a custom base
     URL. Remote Control refuses to start under this wiring, and the
     desktop app ignores it.
   Then size the routes: `$ROUTER settings windows --apply --project-dir
   <project>`. It sets `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (every route's
   context window) to 1000000 and gives each route a `modelSettings`
   `autoCompactWindow`, where it compacts: Codex's default for the GPT routes
   (258400, as input past 272K is billed at a higher rate; 475000 for
   gpt-6.1-sol and gpt-6.1-terra; they accept up to 828400), else its real
   window. It also migrates an earlier setup's lower declaration, and moves
   an entry still at a Codex default that has since changed. When
   it prints `"applied": false`, its `reason` says what needs the user. A
   reason naming the checked-in `.claude/settings.json` as shared: ask
   whether to write there (collaborators get the windows too), or to
   override it in `.claude/settings.local.json` (this checkout only) or
   `~/.claude/settings.json` (every project, while the shared file sets
   neither), then re-run with `--file <that file>`.
   Then list the routes in the `/model` picker: `$ROUTER settings models
   --apply --project-dir <project>`. It edits `modelPicker` in
   `~/.claude/settings.json` (the only file Claude Code reads it from):
   adds the shipped GPT routes' rows with their `modelSettings` windows,
   drops retired GPT rows (GPT-6 Sol, GPT-5.6), keeps every other row, and
   replaces an earlier install's `ANTHROPIC_CUSTOM_MODEL_OPTION` pair. When
   it prints `"applied": false`, its `reason` says what needs the user.
   `--fast` adds the optional Fast twins' rows (1.5–2x faster, 2.5x the usage),
   also offered by the band; add them in setup only when the user asks. Add
   a row per configured Grok, Muse or open-weights route (`routed-models`
   in `$ROUTER doctor`; Muse: per `references/muse.md`, no Full row while
   `muse-plan` warns) by hand, and drop a row when its route goes: rows are
   never checked against the router, and an unserved one fails on its
   first turn. Two cases keep the single-slot pair instead, in the wired
   file's `env` block (one entry only; the other routes stay off the
   picker, reachable through agents or `--model`): project-scoped wiring,
   where user-level rows would show in every project, including ones that
   don't go through the gateway; and Claude Code below 2.1.242 (`claude
   --version`), where the key is unmeasured.
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
   Routing: `claude -p 'reply with ok' --model gpt-6-astra`. Under the
   HTTPS-proxy wiring, also offer `claude remote-control` as a check.
   Picker rows: `/model` in a fresh interactive session lists them after
   the Claude models.
7. Ask whether the user also wants (a) open-weights models (Kimi, GLM, ...)
   served through an OpenAI-compatible host they have an API key for — if
   yes, read `references/open-weights.md` and follow it; (b) Grok models
   under their own xAI subscription login (no API key) — if yes, read
   `references/grok.md` and follow it; (c) Muse (Meta) models under their
   own Muse Code subscription login (no API key) — if yes, read
   `references/muse.md` and follow it; it opens with Meta's terms, which
   the user decides on before anything is enabled. Agents from an earlier
   setup named `<routing-id>(<effort>)` (in `~/.claude/agents/` or the
   project's `.claude/agents/`): offer to replace them following
   `references/custom-agents.md`.

## Moving to the HTTPS-proxy wiring

When doctor's `wiring` check names `ANTHROPIC_BASE_URL` (the session band
offers this too), the rest of the install stays as it is: run the
HTTPS-proxy bullet of step 5 (`trust-ca` on macOS, then `settings proxy
--apply`), which replaces the base URL in place. Same notes as there: say
that macOS asks for a password, mark Linux and WSL as untested, and on a
`reason`, keep the base-URL wiring rather than touching the user's own
proxy settings. Staying on the base URL is fine too; Remote Control and the
desktop app are what it costs. Then step 6's restart and smoke test.

## Repair

`$ROUTER doctor` names the failing layer (config, binary cache, auth,
service, upstream, wiring). Fix only that layer using the matching step above;
`$ROUTER service restart` after config changes. A picker row that fails on its
first turn with "There's an issue with the selected model" names a route the
gateway doesn't serve: compare the rows with doctor's `routed-models`. A
`fallback-model` failure means a Claude Code `fallbackModel` setting is in
effect, which silently re-runs a failed routed subagent on a Claude model;
the fix is removing that setting (then restarting Claude Code), and it is
the user's call.

## Disable / uninstall

1. `$ROUTER settings bypass --project-dir <project>` removes the wiring
   (`ANTHROPIC_BASE_URL`, or `HTTPS_PROXY` with its `proxyAuthHelper`) from
   every settings file that has it, and a `model` key naming a routed ID
   (else new sessions start on a model Anthropic doesn't serve) — this
   alone restores direct Anthropic access. On `"status": "shared-file"` it
   edits nothing: the checked-in project file holds them, so ask before
   removing them there. Optionally remove the other keys step 5 added,
   `modelPicker` included.
2. `$ROUTER service uninstall`. On macOS, `$ROUTER trust-ca --remove` takes
   the router's CA out of the keychain.
3. Optionally delete `~/.config/model-router`, `~/.local/state/model-router`,
   and `~/.cache/model-router` (the state dir includes the Codex auth login —
   warn before deleting).
