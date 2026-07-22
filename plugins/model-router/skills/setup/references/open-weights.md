# Open-weights models via an OpenAI-compatible host

Routes open-weights models (Kimi, GLM, DeepSeek, ...) through the managed
CLIProxyAPI child. Prompts go to the inference host the user picks — with a
US host (OpenRouter, Fireworks, Together, ...), nothing reaches the model
vendor.

1. Ask which models and which host. The host account and API key are the
   user's to create.
2. Append a provider block to `~/.config/model-router/config.toml`. Each
   `[[openai-providers.models]]` entry automatically becomes a routed model —
   never write `[[models]]` entries for these. Example (`$ROUTER
   config-template` shows the commented reference):
   ```toml
   [[openai-providers]]
   name = "openrouter"
   base-url = "https://openrouter.ai/api/v1"

   [[openai-providers.models]]
   name = "moonshotai/kimi-k2.7"
   routing-id = "kimi-k2.7"
   display-name = "Kimi K2.7"
   ```
   The `name` field is the host's exact model ID; treat the example as a
   guess until verified in step 4. Pick short routing-ids — they become the
   `--model` / agent names.
3. API keys live in `~/.config/model-router/secrets.toml` (not the config
   file — the router rejects inline keys). Write the file for the user with
   a placeholder, keyed by provider name, and chmod it 600:
   ```toml
   [openai-providers]
   openrouter = "REPLACE-WITH-YOUR-KEY"
   ```
   Then ask the user to swap in their real key (their own editor; pasting it
   into the chat also works if they don't mind it in the transcript).
4. `$ROUTER verify-providers` — checks every configured model against the
   host's authenticated `/models` endpoint without printing the key. If a
   model reports `missing`, fix its `name` to an exact ID from the host's
   catalog and re-run until everything is `found`.
5. Create a subagent per model so Claude can delegate to it: copy the
   template in `references/custom-agents.md` (omit `effort:` — open-weights
   routes ignore it).
6. `$ROUTER service restart`, then smoke-test every configured routing-id
   with a direct request through the gateway (do NOT use `claude -p
   --model <routing-id>` — Claude Code validates new model IDs against a
   stale list and rejects them):
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"<routing-id>","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming the provider's model ID proves the whole chain. On
   failure, report provider, base-url, model ID, and HTTP status. The
   day-to-day interface is the agents from step 5 (frontmatter `model:`
   accepts any routed ID); they work in sessions started after the restart.
