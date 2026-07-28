# Custom model agents

The plugin ships agents only for the default GPT model x effort combinations.
Any other routed model (open-weights) or effort level gets a user-created
agent — ask where to put it: `~/.claude/agents/` (all projects, usual choice
since the router is global) or the project's `.claude/agents/`.

Template — copy, then substitute the placeholders:

```markdown
---
name: <routing-id>(<effort>)
description: General-purpose agent driven by <Display Name> at <effort> reasoning effort.
model: <routing-id>
effort: <low|medium|high|xhigh>
---
Complete the task you are given.
```

- GPT example: `name: gpt-5.6-sol(low)`, `model: gpt-5.6-sol`,
  `effort: low`, file `gpt-5.6-sol-low.md`.
- Open-weights models: `effort:` is optional. It does reach the host, as
  OpenAI's `reasoning_effort` — every Claude Code level is accepted, including
  ones outside a model's documented set — but how much a level actually
  changes the model's behavior varies by host, so don't promise a user that
  it will. Leaving the line out doesn't disable it either; the session's own
  level is forwarded instead.
- The `model:` value must be a routing ID the router serves (`[[models]]`
  entry or `[[openai-providers.models]]` routing-id); anything else falls
  through to Anthropic and fails with model-not-found.

New agents load at session start — the user must restart sessions to see
them.
