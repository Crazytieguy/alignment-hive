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
   file — the router rejects inline keys). Ask the user to add their key
   there, keyed by provider name, and chmod it 600:
   ```toml
   [openai-providers]
   openrouter = "sk-or-..."
   ```
4. `$ROUTER verify-providers` — checks every configured model against the
   host's authenticated `/models` endpoint without printing the key. If a
   model reports `missing`, fix its `name` to an exact ID from the host's
   catalog and re-run until everything is `found`.
5. Create a subagent per model so Claude can delegate to it: copy the
   template in `references/custom-agents.md` (omit `effort:` — open-weights
   routes ignore it).
6. `$ROUTER service restart`, then smoke-test every configured routing-id:
   `ANTHROPIC_BASE_URL=<base_url> claude -p 'reply with ok' --model
   <routing-id>`. On failure, report provider, base-url, model ID, and HTTP
   status.
