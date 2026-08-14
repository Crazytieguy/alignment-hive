---
name: choosing-models
description: This skill should be read before delegating work to subagents or Workflow agents, unless the user has already named a model. Covers the strengths, cost dynamics, and effort levels of the available models — Claude, GPT, and optionally Grok families — and the mechanics of routing to non-Claude models through model-router.
---

# Choosing models for delegation

model-router makes GPT models available as native Claude Code subagents
alongside Claude models. This skill gives high-level strengths, weaknesses,
and cost dynamics; weigh them against the task at hand rather than following
rules mechanically.

## Cost dynamics

Token price and token *usage* are different axes. At a given effort level,
Opus 5 spends more tokens than gpt-5.6-sol and produces a slightly better
result; Fable's token usage is comparable to Opus 5's, but at twice the
token price and slower. OpenAI subscriptions are also more generous per
dollar, and GPT delegation bills the separate Codex subscription — it
preserves Claude usage entirely. Within the Claude family, Opus 5 costs
half of Fable per token ($5/$25 vs $10/$50) and does not draw from the
Fable-specific usage cap (currently up to ~50% of the weekly limit);
Sonnet 5 is cheaper still ($3/$15, introductory $2/$10 through 2026-08).

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

Opus 5 is by a wide margin the hardest model to prompt-inject measured to
date — relevant when a task involves browsing untrusted websites or
processing untrusted content.

## Model notes

- **Opus 5** — the default Claude choice below Fable, and the pick over
  sol for implementation work where code quality, taste, and judgement
  matter: near-Fable coding and agentic performance at half the token
  price. Also excellent at animations and 3D scenes. Effort: medium for most
  tasks, high for especially difficult ones; above high, gains are
  likely small or negative — each top rung buys about one Artificial
  Analysis index point at 1.3–1.9x the cost, and on scope-penalizing
  evals (FrontierCode) Opus 5 peaks at medium because at higher effort
  it tends to do more than the task asked.
  Hallucinates more than Fable. Verifies its own work unprompted: if
  verification isn't wanted, say so explicitly, and if there's a preferred
  method (run the tests, build, a specific check), name it — otherwise it
  may pick the wrong one.
- **gpt-5.6-sol** — the pick for lower-stakes or tightly specified work:
  faster and cheaper than Opus 5 there and just as capable, and it
  preserves Claude usage. Effort: low and medium are sol's territory —
  prefer sol at medium over Opus 5 at low; on especially difficult tasks
  go high, or hand the task to Opus 5. Sol's benchmark gains above high
  are similarly thin, and its failure mode there is different: users
  report overengineering — over-verifying, testing everything,
  over-defensive code.
- **gpt-5.6-terra** — available, but sol at low or medium is likely the
  better choice: general benchmarks favor sol, and so did a blind review
  bake-off through this integration.
- **gpt-5.6-luna** — truly simple, high-volume mechanical work: reading
  piles of documents, extraction, anything where judgement barely matters.
  Cheaper than Haiku, likely faster, and more capable.
- **Fable** — judgement- and taste-heavy work: design, writing (including
  any copy that persists — prompts, skills, docs), seeing the big picture,
  creative hypothesis generation, and difficult tasks that Opus 5 or sol
  attempted and failed. Mind the Fable usage cap.
- **Sonnet 5** — very high input-token-volume tasks where judgement still
  matters; the intro pricing makes it the cheapest capable Claude.
- **Haiku** — almost never the right call; it's outdated and Sonnet is
  cheap enough.

When the main agent is Fable: implementation subtasks go to Opus 5 when
quality and taste matter, to sol when the task is lower stakes or tightly
specified; judgement-heavy and writing work stays on Fable. Cross-family
review still pays — have a GPT model review Claude work and vice versa.

This guidance draws on broad usage reports; for a recurring use case of
your own, a small blind comparison on the actual task — a Workflow with
anonymized outputs and a judge — settles it.

## Synthesizing independent runs

When the deliverable is a set of findings rather than one holistic
artifact — research, security or code review, test-case design,
conclusions from data — two agents run independently plus a third that
merges their answers reliably beats any single agent: independent runs
cover different ground, and their disagreements mark exactly what the
synthesizer should verify with a few targeted checks. Cross-family
pairs gain the most from the de-correlated strengths above; have the
synthesizer write a standalone answer with no references to its inputs.
For holistic artifacts (implementations, designs, long-form writing),
pick the best candidate and graft ideas from the rest instead.

## Safeguards

Deployment safeguards (bio/cyber classifiers, refusal behavior) differ
sharply per model and per family. When the task might trip them — security
research, biosecurity work, exploit-adjacent fixes, dual-use anything — read
`references/safeguards.md` before choosing; for most tasks it doesn't
matter.

## Grok models (optional)

Like open-weights models, use Grok on the user's request or stated
preference, not autonomously. The usual reasons they ask: lighter
deployment safeguards (see `references/safeguards.md`) and a third billing
pool, preserving both Claude and Codex usage. Use grok-4.6 (a legacy
grok-4.5 route exists for agents that predate it) — it has no measured
edge over sol or Opus 5 here. Give it clear stop conditions and
boundaries, same as the GPT models. Effort: xAI defaults it to high, and
subscription billing makes latency its only cost; it also accepts xhigh.
Its web search returns fewer, title-less results than the Claude and GPT
backends — prefer another family for search-heavy tasks.

## Open-weights models

Open-weights models (Kimi, GLM, ...) are optional; `model-router:setup`
configures the routes and creates their agents. Use models outside the
Claude and GPT families only when the user explicitly asks for them — there
is no strengths/weaknesses guidance for them yet.

## Mechanics

The Agent tool's `model` parameter does not accept GPT models. Use the
shipped agents instead — `gpt-5.6-sol(medium)`, `gpt-5.6-sol(high)`,
`gpt-5.6-terra(high)`, `gpt-5.6-luna(high)` — or, for any other model/effort
combination, Workflow's
`agent(prompt, {model: 'gpt-5.6-sol', effort: 'low'})` — likewise
`{model: 'grok-4.6', effort: 'high'}` when the Grok family is configured.

For Claude models, include the `[1m]` suffix — `fable[1m]`, `sonnet[1m]`,
`opus[1m]` — in agent definitions and Workflow `model` params, or omit
`model` to inherit the parent's. It is harmless when the model already has
its full window, and the difference between 1M and 200K when it doesn't.
