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
- Open-weights models: drop the `effort:` line and the `(<effort>)` suffix
  (the route ignores effort), e.g. `name: kimi-k2.7`, `model: kimi-k2.7`,
  file `kimi-k2.7.md`.
- The `model:` value must be a routing ID the router serves (`[[models]]`
  entry or `[[openai-providers.models]]` routing-id); anything else falls
  through to Anthropic and fails with model-not-found.

New agents load at session start — the user must restart sessions to see
them.
