# Open-weights models via an OpenAI-compatible host

Routes open-weights models (Kimi, GLM, DeepSeek, ...) through the managed
CLIProxyAPI child.

1. Ask which models and which host. The host account and API key are the
   user's to create. A US host (OpenRouter, Fireworks, Together, ...) keeps
   prompts away from the model vendor.
2. Append a provider block to `~/.config/model-router/config.toml`. Each
   `[[openai-providers.models]]` block automatically becomes a routed model —
   never write `[[models]]` entries for these. Example (`$ROUTER
   config-template` shows the commented reference):
   ```toml
   [[openai-providers]]
   name = "openrouter"
   base-url = "https://openrouter.ai/api/v1"

   [[openai-providers.models]]
   name = "moonshotai/kimi-k3"
   routing-id = "kimi-k3"
   display-name = "Kimi K3"
   ```
   The `name` field is the host's exact model ID; treat the example as a
   guess until verified in step 4. Where a model's long-context variant is a
   separate ID, use that one. Pick short routing-ids — they become the
   `--model` / agent names.
3. API keys live in `~/.config/model-router/secrets.toml` (the router rejects
   inline keys in the config). Add a placeholder under `[openai-providers]`,
   keyed by provider name, creating the file with chmod 600 if absent:
   ```toml
   [openai-providers]
   openrouter = "REPLACE-WITH-YOUR-KEY"
   ```
   Ask the user to swap in their real key (their own editor; pasting it into
   the chat also works if they don't mind it in the transcript), and wait
   for them to confirm.
4. `$ROUTER verify-providers` — checks every configured model against the
   host's authenticated `/models` endpoint without printing the key. If a
   model reports `missing`, fix its `name` to an exact ID from the host's
   catalog and re-run until everything is `found`.
5. On OpenRouter, which spreads each model over sub-providers with different
   windows, set `min-context-window = <tokens>` in the model's
   `[[openai-providers.models]]` block to the window wanted (the largest its
   sub-providers serve, per the model's OpenRouter page); sub-providers
   serving less are excluded. A vendor's own host, added as a separate
   `[[openai-providers]]` block, serves one window and needs no pinning.
6. `$ROUTER service restart`, then `$ROUTER doctor` and apply every fix its
   `context-windows` line prints. If it reports a route's real window
   unknown, set `context-window = <tokens>` in that model's block from the
   host's own number, restart, and run doctor again.
7. Create a subagent per model so Claude can delegate to it: copy the
   template in `references/custom-agents.md`.
8. Add `{ "model": "<routing-id>", "label": "<display-name>" }` per route to
   the `modelPicker` options in `~/.claude/settings.json` (user-level wiring
   only).
9. Smoke-test each routing-id through the gateway:
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"<routing-id>","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming the provider's model ID proves the chain. On failure,
   report provider, base-url, model ID, and HTTP status.
