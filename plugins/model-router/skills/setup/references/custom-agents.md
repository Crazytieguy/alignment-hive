# Custom model agents

The plugin ships agents only for the GPT models. Any other routed model
(open-weights, Grok) gets one user-created agent — ask where to put it:
`~/.claude/agents/` (all projects, usual choice since the router is global)
or the project's `.claude/agents/`. Effort is chosen per Agent call, so
offer to replace agents from an earlier setup named `<routing-id>(<effort>)`.

Template — copy, then substitute the placeholders:

```markdown
---
name: <routing-id>
description: General-purpose agent driven by <Display Name>. Read the model-router:delegating-to-models skill before delegating to it.
model: <routing-id>
---
Complete the task you are given.
```

- Open-weights models: effort reaches the host as OpenAI's
  `reasoning_effort` — every Claude Code level is accepted, including ones
  outside a model's documented set — but how much a level actually changes
  the model's behavior varies by host, so don't promise a user that it will.
- The `model:` value must be a routing ID the router serves (`[[models]]`
  entry or `[[openai-providers.models]]` routing-id); anything else falls
  through to Anthropic and fails with model-not-found.

New agents load at session start — the user must restart sessions to see
them.
