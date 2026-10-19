# Muse (Meta) via subscription OAuth

Routes Meta's Muse Wonder models through the same managed CLIProxyAPI child,
under the user's own Muse Code subscription login. No API key. Off by
default; nothing below changes an install without it.

Built-in routes when enabled: `muse-wonder-1.0` (Muse Wonder) and
`muse-wonder-full-1.0` (Muse Wonder Full), both with a ~1M-token window.
Meta serves Wonder Full only on the Power and Maximum Muse Code plans; on
lower plans its requests fail with `model_not_found` (measured on High),
and `$ROUTER doctor`'s `muse-plan` check warns when a Full route is
configured. Standard mode by default; Contributor mode is an opt-in
(below).

## Before enabling: Meta's terms

Show the user this and let them decide; enable nothing until they do.

Meta's Model API terms, §3.2, say the subscription credential "is for use
only with the coding harness under your Coding Harness Subscription ... and
may not be used to access the Services outside the coding harness or to
circumvent your subscription's usage limits or billing." Meta's
subscription docs say the same: the credential "is for use with Muse Code
only." model-router is outside that harness: CLIProxyAPI presents itself to
Meta as Muse Code. Meta lists policy violation among its reasons for
revoking a subscription, which takes effect immediately.

The route Meta documents for third-party coding agents is a
pay-as-you-go Model API key from dev.meta.ai. In
model-router, add it as an OpenAI-compatible provider
(`references/open-weights.md`) with `base-url = "https://api.meta.ai/v1"`,
model `name = "muse-wonder-1.0"` and a routing-id that is not a built-in
Muse ID (for example `meta-wonder`). It serves Wonder only, not Full, is
Standard mode by Meta's docs, and is set up and behaves like any other
open-weights route (`references/open-weights.md`). Untested here.

## Standard or Contributor mode

Contributor mode lets Meta train on prompts, code, files and responses, in
exchange for more usage (Muse Code's own quota prompt puts it at about 2.5x
more turns). Muse Code turns it on by default for subscribers; model-router
does not. Present the choice and let the user pick:

- **Standard** (the default; recommend it to anyone who does not want Meta
  training on their work). model-router sends the plain model IDs and no
  Contributor header, as Muse Code does with the setting off, and refuses
  Muse requests while any Meta login would add the header. Meta's terms
  (§3.3) tie the designation to the account setting, "regardless of which
  Meta Model you select", and that setting lives in Muse Code, so the user
  turns it off there too: install Muse Code
  (`curl -fsSL https://dev.meta.ai/install.sh | sh`; `MUSE_NO_MODIFY_PATH=1`
  leaves shell profiles alone), run `muse --contributor-mode off` outside
  any project (the flag covers that run), sign in through the browser,
  then open `/settings` and turn Contributor mode off. Standard means Meta
  does not train on the content, not that nothing reaches Meta, and
  applies to future requests only.
- **Contributor** (`[muse] contributor = true`). model-router sends the
  `-contributor` model IDs and the Contributor header on every Meta
  request, as Muse Code does with the setting on. Leave Contributor mode
  on in Muse Code (its default). Switching either way takes
  `$ROUTER service restart`, plus the matching Muse Code setting.

## Steps

1. Enable the family in `~/.config/model-router/config.toml`:
   ```toml
   [muse]
   enabled = true
   ```
   plus `contributor = true` if the user chose Contributor.
2. `$ROUTER login muse` — the user must run it themselves (same two options
   as the Codex login: separate terminal, or `!` prefix in the prompt). It
   is a device-code flow: it prints a verification URL and a code, and
   waits. Then `$ROUTER service restart` so the new routes load.
3. `$ROUTER doctor`. A `muse-plan` warning means the plan cannot serve
   Wonder Full: leave Full out of the picker rows and agents below (or
   the user upgrades); `routed-models` lists Full regardless.
4. **Context window**: apply every fix `$ROUTER doctor`'s
   `context-windows` line prints for the Muse routes. They sit at Claude
   Code's 1M cap.
5. **`/model` picker rows**: add to the step-5 `modelPicker` options in
   `~/.claude/settings.json` (the Full row only when `muse-plan` does not
   warn):
   ```json
   { "model": "muse-wonder-1.0", "label": "Muse Wonder" },
   { "model": "muse-wonder-full-1.0", "label": "Muse Wonder Full" }
   ```
   With project-scoped wiring there is only the single-slot env pair; if
   the user prefers Muse in it, replace both values
   (`"ANTHROPIC_CUSTOM_MODEL_OPTION": "muse-wonder-1.0"`,
   `"ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "Muse Wonder"`) and the other
   routes stay off the picker there. Subagents keep working either way.
6. Create an agent per route the plan serves (no Full agent while
   `muse-plan` warns) so Claude can delegate, from the
   template in `references/custom-agents.md`.
7. **Quota**: the subscription has a 5-hour and a weekly window, and a
   long-context request takes a large bite. Measured 2026-10 on the High
   plan in Standard mode: about 40K input tokens per 1% of the 5-hour
   window across a cached Claude Code suite; one near-1M, nearly uncached
   request ran at about 25K per 1%, using 39% of the 5-hour window and 13%
   of the weekly one. Higher plans allow more; Contributor
   mode off is the smaller allowance.
8. Smoke-test with a direct request through the gateway:
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"muse-wonder-1.0","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming a `muse-*` model proves the chain.

If the user asks to remove Muse (or at full uninstall): delete the `[muse]`
block, the Muse picker rows (or the env pair, if it names a Muse ID), a
`model` key naming a Muse ID if a row was saved as the default, and the
Muse agents. The stored login sits in
`~/.local/state/model-router/codex-auth/meta-*.json` and is covered by the
uninstall section's state-dir warning. The Muse Code setting is the user's
own and stays.
