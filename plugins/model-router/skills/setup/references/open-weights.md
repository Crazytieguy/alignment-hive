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
- Setup writes `CLAUDE_CODE_MAX_CONTEXT_TOKENS=258400`, the Codex backend's
  effective input limit behind the GPT routes (272K × 95%).

Every routed ID shares that one number — GPT's, every open-weights
model's, and the Grok routes' if configured. Kimi K3 and GLM-5.2 have 1M-token windows, so a 258400 declaration
clips them to a quarter of their capacity.

**Raising the global value is not an option**, so don't offer it: the shipped
GPT agents and the `choosing-models` skill name the `gpt-5.6-*` IDs,
which would inherit the larger number and start sending the Codex backend
requests past its limit. The declared value stays where the GPT routes need
it.

### The window that counts

The number every option below works from is the window the host
**guarantees**, not the one it advertises: the `guaranteed` figure
`verify-providers` prints. On OpenRouter one model slug is served by
several sub-providers with different windows — Kimi K3 is 1M on most and
8K on one — and routing does not account for prompt size, so a request can
land on the narrowest. The router pins the sub-providers for you: set in
the model's entry
```toml
min-context-window = 1000000
```
and the service routes that model only to sub-providers serving at least
that, re-picking them from OpenRouter's endpoint list at every start;
`verify-providers` shows which it pins and which it excludes. A model with
no qualifying sub-provider is not served rather than served unpinned.
Pointing the entry at one provider's own OpenAI-compatible endpoint
instead — Moonshot, Fireworks and Together each publish one — sidesteps the
question.

### The three options

**A. Leave it alone.** The model is clipped to 258400 and every number Claude
Code displays is true.

**B. Map the picker row to a 1M Claude entry** — for a guaranteed window
of 1M or more (on OpenRouter, `min-context-window = 1000000`). In the
model's `modelPicker` row (step 7) add `"behavesAs": "claude-opus-4-8"`:
```json
{ "model": "kimi-k3", "label": "Kimi K3", "behavesAs": "claude-opus-4-8" }
```
Claude Code then gives that routing ID Opus 4.8's client-side profile, 1M
window included; the request still names the routing ID and goes to the
host. Every displayed number stays true, the other routes keep the declared
window, and the row applies wherever the routing ID is resolved — the
picker, agent definitions, Workflow `agent()` calls — in sessions started
after it is saved. The field is undocumented; if a Claude Code release
drops it, the model falls back to the clipped window. Not together with C
on the same route.

**C. Scale the route's reported usage** — for a guaranteed window between
258400 and 1M, where no Claude entry matches (on OpenRouter,
`min-context-window` = that window). Add to the model's entry:
```toml
context-window-scaling = true
```
The router divides that route's reported usage by `real window / 258400`, so
Claude Code compacts at the model's real limit instead of at 258400 (auto-
compaction fires when the usage reported for a conversation approaches the
believed window, and the router controls the reported numbers). It reads
the real window from the host's catalog at service start and leaves the
route unscaled if what it finds is no larger than 258400 anyway. `doctor`
lists the window it settled on for each route, and fails when a route asked
for scaling and nothing was found — then set `context-window = <tokens>` in
that model's entry, using the host's own number as-is (Claude Code holds
back up to 20000 tokens of the declared window for output, which is roughly
8% of a 1M window once scaled, so it needs no margin subtracted).

Cost: for a scaled route the token counts Claude Code shows are in the
declared coordinate system, not the real one — at 1M real tokens the meter
reads 258400 and calls it full. Percentages stay right; absolute token counts
and that route's cost telemetry do not. Nothing outside the router config
changes.

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

Re-run `$ROUTER verify-providers`, `$ROUTER service restart` (the service
discovers each route's guaranteed window at start and doctor reads what it
found), and `$ROUTER doctor`.
Env changes need a Claude Code restart; config changes need
`$ROUTER service restart`.
