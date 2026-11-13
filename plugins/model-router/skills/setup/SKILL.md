---
name: setup
description: Set up, configure, verify, repair, or uninstall model-router, the local gateway that routes Claude Code to non-Claude models, including any of its config options — binary install, CLIProxyAPI, Codex OAuth or OpenAI API key, optional Grok (xAI), Muse (Meta) and open-weights models, Ultrafast GPT models, OS service, and Claude Code settings wiring.
---

# model-router setup

`ROUTER="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"` — every command below
goes through it (it resolves the pinned router binary on first use). This
flow is idempotent; `$ROUTER doctor` at any point shows what's left to do.
macOS and Linux only.

## Wrong platform (check this first)

`$ROUTER platform-check` — silent and exit 0 means all is well, go on to the
section the request needs (the install flow for a new setup). It only fails
when a platform-specific plugin entry for a *different* machine is enabled:
those bundle one target's binary, so the wrong one cannot run. It prints the entry that should be there, e.g.
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

1. **Diagnose**: `$ROUTER doctor`. Go through the steps below, but run a
   step only where doctor or the settings it names show it isn't done,
   and don't ask again what the user already answered (an existing
   setting or config entry is their answer). `settings windows` and
   `settings models` in step 5 change nothing when there is nothing to
   update, so run them on every pass. On an install
   that already works, skip step 7's offers unless the user asks. When
   the user asks about a particular step or option, or to redo one, go
   to it directly.
2. **Upstream binary**: `$ROUTER ensure-upstream` — downloads the pinned,
   checksum-verified CLIProxyAPI release into the cache. No-op when present.
3. **GPT auth**: `$ROUTER doctor` reports auth state. If it found and
   imported an existing CLIProxyAPI Codex login, say so and move on. If not,
   ask whether the user will sign in with a ChatGPT subscription
   (recommended) or use an OpenAI API key (billed per token). For the
   subscription, the user must run the interactive login themselves (it
   opens a browser for Codex OAuth). Either paste the full expanded
   `.../scripts/bootstrap.sh login` command for them to run in a separate
   terminal, or have them type `! $ROUTER login` (expanded to the real path)
   in the prompt. For an API key, write `auth = "api-key"` under `[gpt]` in
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
5. **Wire Claude Code**: make the router Claude Code's HTTPS proxy,
   without asking first or offering a choice of wiring ("Wirings" below
   has the answers if the user asks). The exception is an install that
   doctor's `wiring` check names `ANTHROPIC_BASE_URL`: that wiring is the
   user's earlier choice, so mention in a line that moving to the proxy
   keeps Remote Control on and routes the desktop app, move it per
   "Wirings" if they want, and otherwise skip to sizing the routes.

   If the user's settings or environment already set a proxy, a
   `NO_PROXY` covering `api.anthropic.com`, or an `ANTHROPIC_BASE_URL` of
   their own, settle that per "Wirings" first. On macOS, then `$ROUTER
   trust-ca`, telling the user beforehand that macOS
   may ask for their password. Then `$ROUTER settings proxy --apply
   --project-dir <project>`, which wires `~/.claude/settings.json`; when
   the plugin is enabled only in the project's settings, add `--file
   <project>/.claude/settings.local.json`. On `"applied": false`, its
   `reason` says what is wrong: fix that and run it again, but ask before
   changing a proxy, CA or gateway setting of the user's own ("Wirings"
   covers the user keeping it). Then tell the user, in a sentence, that
   while the router is down Claude Code and the programs it runs have no
   network; the notice model-router shows at the next start or `/clear`
   then offers Bypass router (followed by a Claude Code restart). On Linux
   and WSL, also say this wiring is untested.

   Then size the routes: `$ROUTER settings windows --apply --project-dir
   <project>`. It sets `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (every route's
   context window) to 1000000 and gives each route a `modelSettings`
   `autoCompactWindow`, where Claude Code compacts: Codex's own default
   for the GPT routes, else the route's real window. It also updates
   entries an earlier setup wrote. When
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
   replaces rows of GPT models that are no longer shipped, keeps every
   other row, and replaces an earlier install's
   `ANTHROPIC_CUSTOM_MODEL_OPTION` pair. When
   it prints `"applied": false`, its `reason` says what needs the user.
   `--fast` adds the optional Fast twins' rows (1.5–2x faster; 2x the
   usage on the 6.2 models, 2.5x on Terra and Luna),
   also offered by the session-start notice; add them in setup only when
   the user asks.
   Ultrafast (GPT-6.2 Sol and GPT-6.2 Astra, up to 4.5x faster for 6x the
   usage) needs a plan that has it: Pro $200 (Sol only), Pro $500, some
   Enterprise and Edu, or an API key (at API Ultrafast prices; the router
   doesn't check the key's access). With a managed CLIProxyAPI the
   router always serves the `-ultrafast` routes and refuses a twin's
   turns when the Codex plan lacks the tier for it. `$ROUTER settings
   ultrafast --check` reads the plan, then prints `"needed": true` with
   `labels`, the twins this plan has, when there is something to set up:
   on a subscription, offer them; on an API key, only when the user asks.
   `--apply` adds `-ultrafast` rows after their bases, their windows, and
   user agents in `~/.claude/agents`. Its `plan` says why
   nothing is
   needed: `lists` (already set up), `lacks` (the plan doesn't have the
   tier), `unread` (the read failed, as printed: retry once the service
   runs with a working Codex login), `unserved` (an external CLIProxyAPI,
   which gets no `-ultrafast` routes). Add
   a row per configured Grok, Muse or open-weights route (`routed-models`
   in `$ROUTER doctor`; Muse: per `references/muse.md`, no Full row while
   `muse-plan` warns) by hand, and drop a row when its route goes: rows are
   never checked against the router, and an unserved one fails on its
   first turn. Two cases keep the single-slot pair instead, in the wired
   file's `env` block (one entry only; the other routes stay off the
   picker, reachable through agents or `--model`): project-scoped wiring,
   where user-level rows would show in every project, including ones that
   don't go through the gateway; and Claude Code below 2.1.242 (`claude
   --version`), where `modelPicker` is untested.
   ```json
   "ANTHROPIC_CUSTOM_MODEL_OPTION": "gpt-6.2-astra",
   "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "GPT-6.2 Astra"
   ```
6. Tell the user to restart Claude Code sessions (settings are read at
   startup), and that the GPT agents and `delegating-to-models` skill are now
   available.
   Offer to run smoke tests. A fresh `claude -p` reads the step 5 settings
   at its own startup, so they work without restarting the current session.
   Don't env-prefix the step 5 variables onto them: once the settings env
   block exists it silently overrides shell-provided values.
   Routing: `claude -p 'reply with ok' --model gpt-6.2-sol`. Under the
   HTTPS-proxy wiring, also offer `claude remote-control` as a check.
   Picker rows: `/model` in a fresh interactive session lists them after
   the Claude models.
   When `$CLAUDE_CODE_ENTRYPOINT` is `claude-desktop` (this session runs
   in the desktop app), tell the user that the app's model menu lists only
   Claude models, and that typing `/model <routing-id>` in the message box
   picks a route.
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
8. **Auto-mode classifier (experimental, ask once).** Skip this step when
   `~/.config/model-router/config.toml` already has an
   `[auto-mode-classifier]` section: the user has answered. Also skip it
   while `$ROUTER doctor` fails `router-version`, until `$ROUTER service
   refresh` fixes that: a router older than this plugin refuses a config
   with the section and does not start. Otherwise ask, saying it is
   experimental. In auto mode, Anthropic's server checks Claude's tool
   calls for free, but once a session has a GPT reply with tool calls,
   Claude Code checks those calls and every one after them in the session
   with Sonnet on the user's Claude plan. With this on, the router checks those GPT tool calls
   on GPT (Luna, then Sol for any Luna doesn't clear), and the session
   keeps the free check. It pays off most for heavy Luna or Haiku users:
   their sessions make many tool calls for the usage they otherwise drain,
   so the Sonnet checks are a large share of it. The costs: it is
   sometimes stricter than Claude Code's own check, a call it gets no
   verdict on is denied (the agent may retry it), and a call Luna doesn't
   clear waits several seconds longer. Write the answer either way, so the
   next run doesn't ask again:
   ```toml
   [auto-mode-classifier]
   enabled = true
   ```
   (`enabled = false` for no), then `$ROUTER service restart` and `$ROUTER
   doctor`, whose `auto-mode-classifier` line shows it on.

## Wirings

Claude Code reaches the router in one of two ways. Explain them when the
user asks:

- **HTTPS proxy**, what step 5 sets up. Claude Code keeps talking to
  `api.anthropic.com`, so Remote Control and the desktop app work. The
  router decrypts only Claude Code's own `api.anthropic.com` traffic, with
  a local CA that can vouch for no other host; everything else passes
  through undecrypted. While the router is down, Claude Code and the
  programs it runs have no network: the service restarts it, and the
  notice model-router shows offers Bypass router (`$ROUTER settings
  bypass`). Untested on Linux and WSL.
- **Base URL** (`ANTHROPIC_BASE_URL`), the original wiring. Remote Control
  refuses to start under it, and the desktop app ignores it. It needs no
  CA, and while the router is down only Claude Code's own API calls fail.

**Falling back to the base URL.** Only when the user keeps a proxy,
`NO_PROXY` or CA setting of their own that the proxy wiring conflicts
with (not another gateway's `ANTHROPIC_BASE_URL`, which this would
replace: if the user keeps that, tell them model-router stays unwired and
stop the install flow there). By hand, add to the `env` block of the file
step 5 would have written, with `.base_url` from `$ROUTER doctor --json`:
```json
"ANTHROPIC_BASE_URL": "<base_url from doctor --json>",
"_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL": "1",
"CLAUDE_CODE_GATEWAY_HINT_HEADERS": "1"
```
Tell the user what this wiring costs (above), and continue step 5 from
sizing the routes.

**Moving a base-URL install to the proxy.** When the user wants it (step
5 mentions it; staying is fine), wire as in step 5, with no `--file`: the
proxy wiring replaces the base URL where it is. The command won't edit the
checked-in `.claude/settings.json`: with the user's consent, take the
three keys above out of it by hand, then wire as for a new install. Then
step 6's restart and smoke test.

## Configuration

`$ROUTER config-template` prints the annotated template of every
`~/.config/model-router/config.toml` option. Options with a step above
(GPT auth, Grok, Muse, open-weights providers, the auto-mode classifier)
follow that step. After any other change, run `$ROUTER doctor` before
restarting (its `config` check catches a file the service would refuse to
start with), then `$ROUTER service restart` and `$ROUTER doctor` again.

## Repair

`$ROUTER doctor` names the failing layer (config, binary cache, auth,
service, upstream, wiring). Fix only that layer using the matching step above;
`$ROUTER service restart` after config changes. A picker row that fails on its
first turn with "There's an issue with the selected model" names a route the
gateway doesn't serve: compare the rows with doctor's `routed-models`. A
`fallback-model` failure means a Claude Code `fallbackModel` setting is in
effect, which silently re-runs a failed routed subagent on a Claude model;
the fix is removing that setting (then restarting Claude Code), and it is
the user's call. For a `models-block` warning, offer to make the rename it
describes.

## Disable / uninstall

1. `$ROUTER settings bypass --project-dir <project>` takes the wiring, and
   a `model` key naming a routed ID, out of every settings file; this
   alone restores direct Anthropic access once Claude Code restarts. On
   `"status": "shared-file"` it edits nothing: the checked-in project file
   holds them, so ask before removing them there. Optionally remove the
   other keys step 5 added, and the routed rows from `modelPicker` (the
   user's other rows stay).
2. Delete the agents whose routes go with the router: the Ultrafast
   agents it wrote (files in `~/.claude/agents` whose frontmatter has the
   line `# Written by model-router`), and the Grok, Muse and open-weights
   agents setup created (in `~/.claude/agents` or a project's
   `.claude/agents`, with a `model:` naming a `grok-` or `muse-` route,
   or a routing-id under `[[openai-providers.models]]` in
   `~/.config/model-router/config.toml`). Ask before deleting an agent
   the user may have written or edited.
3. `$ROUTER service uninstall`; on macOS, `$ROUTER trust-ca --remove`
   takes the router's CA out of the keychain. Under the HTTPS-proxy
   wiring this session keeps going through the router until Claude Code
   restarts, so once the service stops it has no connection: say so, and
   give the user these commands (and step 4's) to run themselves, or run
   them as your last action.
4. Optionally delete `~/.config/model-router`, `~/.local/state/model-router`,
   and `~/.cache/model-router` (the state dir includes the Codex auth login —
   warn before deleting).
