# Grok (xAI) via subscription OAuth

Routes Grok models through the same managed CLIProxyAPI child, under the
user's own xAI subscription login (SuperGrok, or an X account with Premium —
the login itself is the test; it costs nothing to try). No API key. Off by
default; nothing below changes a GPT-only install.

Built-in route when enabled: `grok-4.6` (500K-token window). A legacy
`grok-4.5` route is also registered so configs and agents that predate
4.6 keep working.

1. Enable the family in `~/.config/model-router/config.toml`:
   ```toml
   [grok]
   enabled = true
   ```
2. `$ROUTER login grok` — the user must run it themselves (same two options
   as the Codex login: separate terminal, or `!` prefix in the prompt). It
   is a device-code flow, not a browser callback: it prints a verification
   URL and a code, and waits. The running service picks the credential up
   without a restart, but restart anyway so the new routes load:
   `$ROUTER service restart`.
3. `$ROUTER doctor` — `grok-auth` confirms the login, `routed-models`
   confirms the live child actually serves the Grok ID.
4. **Context window**: Claude Code decides a model's window client-side
   from the model ID; for routed IDs the one global
   `CLAUDE_CODE_MAX_CONTEXT_TOKENS` declaration applies, and it is sized
   to 258400 for the GPT routes (the Codex backend's input limit — raising
   it would push those routes past it). So the Grok routes are clipped to
   258400 of their real 500K windows unless their reported usage is rescaled:
   ```toml
   [grok]
   enabled = true
   context-window-scaling = true
   ```
   With scaling, the router divides each route's reported usage by
   `500000 / 258400`, so auto-compaction fires near the real window; the
   cost is that Claude Code's displayed token counts for Grok routes read
   low by that ratio (percentages stay right). Recommend scaling — the
   larger window is much of the point — but flip it only with the user's
   explicit OK after stating that displayed-count caveat.
5. **`/model` picker row**: add to the step-5 `modelPicker` options in
   `~/.claude/settings.json`:
   ```json
   { "model": "grok-4.6", "label": "Grok 4.6" }
   ```
   With project-scoped wiring there is only the single-slot env pair; if
   the user prefers Grok in it, replace both values
   (`"ANTHROPIC_CUSTOM_MODEL_OPTION": "grok-4.6"`,
   `"ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "Grok 4.6"`) and GPT stays off
   the picker there. Subagents for both families keep working either way.
6. Create agents so Claude can delegate — use the template in
   `references/custom-agents.md`. Recommended set: `grok-4.6(high)` (xAI's
   own default effort) and optionally `grok-4.6(medium)` for faster runs
   (`model: grok-4.6`, `effort: high`/`medium`). Effort comes from agent
   frontmatter exactly like the GPT agents — the router translates it for
   xAI.
7. Smoke-test with a direct request through the gateway:
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"grok-4.6","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming a `grok-*` model proves the chain (the served name
   carries a `-build` suffix — grok-4.6 answers as `grok-4.6-build` —
   that's expected).

If the user asks to remove Grok (or at full uninstall): delete the `[grok]`
block, the Grok picker row (or the env pair, if it names a Grok ID), and a
`model` key naming a Grok ID if a row was saved as the default. The stored
login sits in `~/.local/state/model-router/codex-auth/xai-*.json` and is
covered by the uninstall section's state-dir warning.
