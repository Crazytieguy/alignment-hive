---
name: choosing-models
description: Use when delegating work to subagents or Workflow agents and choosing which model and effort level to run them on — covers Claude vs GPT model strengths, cost dynamics, effort levels, and the mechanics of routing to GPT models through model-router.
---

# Choosing models for delegation

model-router makes GPT models available as native Claude Code subagents
alongside Claude models. This skill gives high-level strengths, weaknesses,
and cost dynamics; weigh them against the task at hand rather than following
rules mechanically.

## Cost dynamics

Token price and token *usage* are different axes. GPT-5.6 models complete a
typical task in fewer tokens than Claude models, so they are cheaper per
task than the price sheet suggests — a typical Fable task at xhigh effort
costs more than twice gpt-5.6-sol at max effort. The same pattern holds for
Sonnet and, less strongly, Opus. OpenAI subscriptions are also more generous
per dollar, and GPT delegation bills the separate Codex subscription — it
preserves Claude usage, which matters because Claude subscriptions cap
Fable-specific usage (currently up to ~50% of the weekly limit).

## Character differences

The Claude constitution treats the model as an entity expected to infer the
intent behind a request: Claude models (Fable especially) fill in unstated
requirements, notice contradictions and surface them, and take some liberty
interpreting what you meant. The OpenAI model spec produces the opposite
temperament: GPT models execute instructions straightforwardly and precisely,
and make fewer mechanical mistakes doing so. For a fully specified task,
gpt-5.6-sol can be *more* reliable than Fable.

The flip side: GPT-5.6 models reward-hack partial instructions. Told to "make
sure it works" without network access to a dependency, sol has been observed
reimplementing the dependency from scratch rather than escalating; they also
sometimes work around guardrails and constraints to complete the task. So:
fully specify the desired outcome and constraints — including what not to do
and which side effects are acceptable; state explicitly that being blocked
should be reported back rather than worked around; and avoid handing GPT
models tasks that require broad access to sensitive resources.

Claude and GPT models also have de-correlated strengths and weaknesses: the
mistakes one family makes, the other tends to catch. For best results, have a
Claude model review GPT work and vice versa.

## Model notes

- **gpt-5.6-sol** — the GPT workhorse and the default; best
  intelligence-per-cost of the family. Effort: medium is a good default;
  high suits long tasks needing self-verification loops; above high is
  rarely worth it — the model overthinks and overengineers.
- **gpt-5.6-terra** — available, but sol at low or medium is likely the
  better choice: general benchmarks favor sol, and so did a blind review
  bake-off through this integration.
- **gpt-5.6-luna** — truly simple, high-volume mechanical work: reading
  piles of documents, extraction, anything where judgement barely matters.
  Cheaper than Haiku, likely faster, and more capable.
- **Fable** — judgement- and taste-heavy work: design, writing, creative
  hypothesis generation, and difficult tasks that sol attempted and failed.
  Mind the Fable usage cap.
- **Sonnet 5 / Opus 4.8** — roughly on par: Opus makes fewer mistakes and is
  more reliable; Sonnet writes a little more nicely, asks good questions, and
  delegates well. Use them where many tokens will be spent but the task isn't
  specified tightly enough to hand to a GPT model, or where reward-hacking is
  a concern.
- **Haiku** — almost never the right call; it's outdated and Sonnet is cheap
  enough.

When the main agent is Fable, most delegation should go to GPT models
(well-specified subtasks, review) or Fable itself (judgement-heavy work), with
Sonnet/Opus covering the underspecified-but-token-heavy middle.

Before diverging from the sol default for a recurring use case, run a small
blind comparison on the actual task — a Workflow with anonymized outputs and
a judge settles it cheaply.

## Mechanics

The Agent tool's `model` parameter does not accept GPT models. Use the
shipped agents instead — `gpt-5.6-sol(medium)`, `gpt-5.6-sol(high)`,
`gpt-5.6-terra(high)`, `gpt-5.6-luna(high)` — or, for any other model/effort
combination, Workflow's
`agent(prompt, {model: 'gpt-5.6-sol', effort: 'low'})`. Prefer the bare
`gpt-5.6-*` routing IDs over the `claude-gpt-5.6-*` aliases: only the bare
IDs pick up the declared GPT context window
(`CLAUDE_CODE_MAX_CONTEXT_TOKENS`).
