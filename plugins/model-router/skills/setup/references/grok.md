# Grok (xAI) via subscription OAuth

Routes Grok models through the same managed CLIProxyAPI child, under the
user's own xAI subscription login (SuperGrok, or an X account with Premium —
the login itself is the test; it costs nothing to try). No API key. Off by
default; nothing below changes a GPT-only install.

Built-in routes when enabled: `grok-4.5` / `claude-grok-4.5` (500K-token
window).

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
   from the model ID; for bare routing IDs the one global
   `CLAUDE_CODE_MAX_CONTEXT_TOKENS` declaration applies, and it is sized
   to 258400 for the GPT routes (the Codex backend's input limit — raising
   it would push those routes past it). So bare `grok-4.5` is clipped to
   258400 of its real 500K window unless its reported usage is rescaled:
   ```toml
   [grok]
   enabled = true
   context-window-scaling = true
   ```
   With scaling, the router divides the route's reported usage by
   `500000 / 258400`, so auto-compaction fires near the real window; the
   cost is that Claude Code's displayed token counts for Grok routes read
   low by that ratio (percentages stay right). Recommend scaling — the
   larger window is much of the point — but flip it only with the user's
   explicit OK after stating that displayed-count caveat.
5. **Main-model slot (only if the user wants Grok in the /model picker)**:
   Claude Code has a single custom-model slot, and the main setup already
   spends it on `gpt-5.6-sol`. If the user prefers Grok there, replace both
   env values in the step-5 settings block:
   ```json
   "ANTHROPIC_CUSTOM_MODEL_OPTION": "grok-4.5",
   "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME": "Grok 4.5"
   ```
   One slot means one of GPT/Grok; the other family stays subagent-only.
   Whichever is chosen, subagents for both keep working.
6. Create agents so Claude can delegate — use the template in
   `references/custom-agents.md`. Recommended set: `grok-4.5(high)` (xAI's
   own default effort) and optionally `grok-4.5(medium)` for faster runs
   (`model: grok-4.5`, `effort: high`/`medium`). Effort comes from agent
   frontmatter exactly like the GPT agents — the router translates it for
   xAI.
7. Smoke-test with a direct request through the gateway:
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"grok-4.5","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming a `grok-*` model proves the chain (grok-4.5 is served
   as `grok-4.5-build` — that's the expected name).

If the user asks to remove Grok (or at full uninstall): delete the `[grok]`
block, and the custom-model env pair if it names a Grok ID. The stored
login sits in `~/.local/state/model-router/codex-auth/xai-*.json` and is
covered by the uninstall section's state-dir warning.
