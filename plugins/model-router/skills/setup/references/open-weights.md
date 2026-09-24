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
   host's authenticated `/models` endpoint without printing the key. If a
   model reports `missing`, fix its `name` to an exact ID from the host's
   catalog and re-run until everything is `found`.
5. **Settle context windows** — read the section below and walk the user
   through the choice.
6. Create a subagent per model so Claude can delegate to it: copy the
   template in `references/custom-agents.md`.
7. `$ROUTER service restart`, then smoke-test every configured routing-id
   with a direct request through the gateway — curl rather than `claude -p
   --model <routing-id>`, because curl isolates the router→provider chain
   from Claude Code's own wiring (`claude -p` also works once the restarted
   service knows the route, provided the main setup's settings env block
   points Claude Code at the gateway):
   ```
   curl -s <base_url from doctor --json>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"<routing-id>","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A response naming the provider's model ID proves the whole chain. On
   failure, report provider, base-url, model ID, and HTTP status. The
   day-to-day interface is the agents from step 6 (frontmatter `model:`
   accepts any routed ID); they work in sessions started after the restart.
   Add a row per routing-id to the main setup's step-5 `modelPicker` list
   (user-level wiring only; `display-name` makes a good label).

## Context windows

### Why there is a tradeoff at all

Claude Code decides a model's context window **client-side, from the model
ID**. The router has no say, and no API response can tell it otherwise:

- A model ID Claude Code doesn't recognize gets **200000** tokens.
- `CLAUDE_CODE_MAX_CONTEXT_TOKENS` overrides that, but it is **one global
  value** and it is **ignored for any ID starting with `claude-`**.
- Setup writes `CLAUDE_CODE_MAX_CONTEXT_TOKENS=258400`, Codex's own default
  window for the GPT routes (272K × 95%).

Every routed ID shares that one number. Kimi K3 and GLM-5.2 have 1M-token
windows, so a 258400 declaration clips them to a quarter of their capacity.

**Don't raise the global value to fit an open-weights model**: every
`gpt-*` route inherits it, GPT input past 272K is billed at a higher rate,
and the GPT routes stop at 828400 anyway.

### On OpenRouter, pin the window first

OpenRouter spreads each model over several sub-providers with different
windows. Set in the model's entry
```toml
min-context-window = 1000000
```
to exclude the sub-providers that don't serve that window. The options
below then work from that number (on other hosts, from the window
`verify-providers` reports). Or point the entry at one provider's own
endpoint — Moonshot, Fireworks and Together each publish one — and skip
this.

### The three options

**A. Leave it alone.** The model is clipped to 258400 and every number Claude
Code displays is true.

**B. Map the picker row to a 1M Claude entry** — for a 1M window. In the
model's `modelPicker` row (step 7) add `"behavesAs": "claude-opus-4-8"`:
```json
{ "model": "kimi-k3", "label": "Kimi K3", "behavesAs": "claude-opus-4-8" }
```
Claude Code then gives that routing ID Opus 4.8's client-side profile, 1M
window included; the request still names the routing ID and goes to the
host. The row applies wherever the routing ID is resolved — the picker,
agent definitions, Workflow `agent()` calls — in sessions started after it
is saved. The field is undocumented; if a Claude Code release drops it, the
model falls back to the clipped window. Not together with C on the same
route.

**C. Scale the route's reported usage** — for a window between 258400 and
1M. Add to the model's entry:
```toml
context-window-scaling = true
```
The router divides that route's reported usage by `real window / 258400`, so
Claude Code compacts at the model's real limit instead of at 258400. The
real window comes from the host at service start; if `doctor` says none
was found, set `context-window = <tokens>` in the entry, using the host's
own number as-is (no margin).

Cost: the token counts Claude Code shows for a scaled route are in the
declared coordinate system, not the real one — at 1M real tokens the meter
reads 258400 and calls it full. Percentages stay right; absolute token
counts and that route's cost telemetry do not.

When moving a route from B to C, end the sessions that loaded the row
before enabling scaling: a session that believes the row's window while
the router scales that route overruns.

### Models whose window is *below* 258400

Scaling cannot help here and the router rejects it: reporting more tokens
than were really used would still miss, because Claude Code mixes in its own
unscaled estimate of the newest messages. Lower
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` to that model's window instead. The GPT
routes then believe the lower number too and are simply clipped — safe, and
recoverable by giving them a `[[models]]` list with `context-window-scaling`
of their own if the user wants their full window back.

### After applying the choice

`$ROUTER service restart`, then `$ROUTER verify-providers` and `$ROUTER
doctor`. Env changes need a Claude Code restart.
