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
   name = "moonshotai/kimi-k3"
   routing-id = "kimi-k3"
   display-name = "Kimi K3"
   ```
   The `name` field is the host's exact model ID; treat the example as a
   guess until verified in step 4. Watch for models whose long-context
   variant is a separate ID (GLM-5.2's 1M window is `glm-5.2[1m]`; the base
   ID serves less). Pick short routing-ids — they become the `--model` /
   agent names.
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
   host's authenticated `/models` endpoint without printing the key, and
   reports each model's context window as the host advertises it. If a model
   reports `missing`, fix its `name` to an exact ID from the host's catalog
   and re-run until everything is `found`. The reported windows are what the
   next step is about.
5. **Settle context windows** — read the section below and walk the user
   through the choice.
6. Create a subagent per model so Claude can delegate to it: copy the
   template in `references/custom-agents.md`.
7. `$ROUTER service restart`, then smoke-test every configured routing-id
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
   day-to-day interface is the agents from step 6 (frontmatter `model:`
   accepts any routed ID); they work in sessions started after the restart.

## Context windows

### Why there is a tradeoff at all

Claude Code decides a model's context window **client-side, from the model
ID**. The router has no say, and no API response can tell it otherwise:

- A model ID Claude Code doesn't recognize gets **200000** tokens.
- `CLAUDE_CODE_MAX_CONTEXT_TOKENS` overrides that, but it is **one global
  value** and it is **ignored for any ID starting with `claude-`**.
- Setup writes `CLAUDE_CODE_MAX_CONTEXT_TOKENS=250000`, sized for the Codex
  backend behind the GPT routes (~258K effective input).

Every bare routing ID shares that one number — GPT's and every open-weights
model's. Kimi K3 and GLM-5.2 have 1M-token windows, so a 250000 declaration
clips them to a quarter of their capacity.

**Raising the global value is not an option**, so don't offer it: the shipped
GPT agents and the `choosing-models` skill name the bare `gpt-5.6-*` IDs,
which would inherit the larger number and start sending the Codex backend
requests past its limit. The declared value stays where the GPT routes need
it.

That same believed window drives **auto-compaction** — Claude Code compacts
when the usage reported for a conversation approaches it — and the router
controls the reported numbers. That is what the second option below uses.

### The two options

**A. Leave it alone.** The model is clipped to 250000 and every number Claude
Code displays is true.

**B. Scale the route's reported usage.** Add to the model's entry:
```toml
context-window-scaling = true
```
The router divides that route's reported usage by `real window / 250000`, so
Claude Code compacts at the model's real limit instead of at 250000. It reads
the real window from the host's catalog at startup and leaves the route
unscaled if what it finds is no larger than 250000 anyway. `doctor` lists the
window it settled on for each route, and fails when a route asked for scaling
and nothing was found — then set `context-window = <tokens>` in that model's
entry, using the host's own number as-is (Claude Code holds back up to 20000
tokens of the declared window for output, which is roughly 8% of a 1M window
once scaled, so it needs no margin subtracted).

On OpenRouter one model slug is served by several sub-providers with
different windows — Kimi K3 is 1M on most and 8K on one — and routing does not
account for prompt size, so the router scales against the narrowest, which
usually means it declines to scale at all. Pinning is worth it: in
`https://openrouter.ai/settings/privacy` the user sets account-wide **allowed
providers** (or ignores the narrow one), which applies to every API request.
The router reads OpenRouter's public catalog and cannot see those account
settings, so after pinning, set `context-window` in the entry by hand to the
window the pinned providers serve. Pointing the entry at one provider's own
OpenAI-compatible endpoint instead — Moonshot, Fireworks and Together each
publish one — gets the same result without the dashboard.

Cost: for a scaled route the token counts Claude Code shows are in the
declared coordinate system, not the real one — at 1M real tokens the meter
reads 250000 and calls it full. Percentages stay right; absolute token counts
and that route's cost telemetry do not. Nothing outside the router config
changes.

### Models whose window is *below* 250000

Scaling cannot help here and the router rejects it: reporting more tokens
than were really used would still miss, because Claude Code mixes in its own
unscaled estimate of the newest messages. Lower
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` to that model's window instead. The GPT
routes then believe the lower number too and are simply clipped — safe, and
recoverable by giving them a `[[models]]` list with `context-window-scaling`
of their own if the user wants their full window back.

### After applying the choice

Re-run `$ROUTER verify-providers` and `$ROUTER doctor`. Doctor reports the
client window it resolved, each route's status — matched, clipped, scaled, or
`OVERRUN RISK` — and flags a running service whose resolved value is stale.
Env changes need a Claude Code restart; config changes need
`$ROUTER service restart`.
