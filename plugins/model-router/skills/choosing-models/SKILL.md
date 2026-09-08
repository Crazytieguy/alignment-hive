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

Token price and token *usage* are different axes; per-task cost is their
product, and the models below differ on both. Under subscriptions the
billing pool matters more than list price: GPT delegation bills the
separate Codex subscription and preserves Claude usage entirely, and OpenAI
subscriptions are more generous per dollar. Within the Claude family, Fable
draws from its own usage cap (currently up to ~50% of the weekly limit)
that Opus 5 and Sonnet 5 don't touch.

## Character differences

The Claude constitution treats the model as an entity expected to infer the
intent behind a request: Claude models (Fable especially) fill in unstated
requirements, notice contradictions and surface them, and take some liberty
interpreting what you meant. The OpenAI model spec produces a more literal
temperament: GPT models execute instructions as written and make fewer
mechanical mistakes doing so, so a fully specified task can go better on a
GPT model than on Fable.

What "literal" looks like differs by generation. GPT-5.6 tends to
over-persist: told to "make sure it works" without access to a dependency,
sol has reimplemented the dependency rather than escalating, and the family
sometimes works around constraints to finish. OpenAI describes GPT-6 Astra
as the opposite — it stops to ask when an answer could change the result,
may hand back a first implementation for review, and is more sensitive to
instructions in skills and CLAUDE.md files. That is the vendor's own
characterization plus a few days of reports, not a settled picture. The same
prompt hygiene serves either way: state the desired outcome, the
constraints, what counts as done, and whether being blocked should be
reported back or worked around.

Claude and GPT models have de-correlated strengths and weaknesses: the
mistakes one family makes, the other tends to catch. For best results, have
a Claude model review GPT work and vice versa.

Claude models are much harder to prompt-inject than GPT models, and the
gap is between families rather than within them: on the Gray Swan
indirect-injection benchmark Opus 5 and Fable 5.1 sit near 2% attack
success over 15 attempts, GPT-6 Astra at 8.5% (down from 27% for
GPT-5.6). Claude Code's auto mode adds its own classifier pass over tool
calls for whichever model is running, cutting the risk below these
numbers for all of them. Relevant when a task involves browsing untrusted
websites or processing untrusted content.

## Model notes

- **Fable** — judgement- and taste-heavy work: design and front-end,
  writing (including any copy that persists — prompts, skills, docs),
  seeing the big picture, creative hypothesis generation, and difficult
  tasks another model attempted and failed. Also best for simple, clean
  code and for simplifying existing code. Effort: medium for most tasks.
  At high and above it does more on its own — extra verification,
  proactive edits — and token use climbs with it. Mind the Fable usage
  cap.
- **gpt-6-astra** — the default for implementation and agentic work that
  doesn't need Fable's strengths. Priced like Fable ($10/$50) but reported
  to spend a fraction of the tokens, so per-task cost lands well below it,
  and it bills Codex. Its clearest gains over other models are computer
  use and 3D work (independently confirmed, including from inside Claude
  Code), plus OpenAI's reported long autonomous runs and research-level
  math; on coding benchmarks it leads Fable 5.1 by a few points on
  terminal and agentic work (Terminal-Bench 4.0, DeepSWE, Code Arena),
  ties it on FrontierCode, and third-party composites split; early
  reports put it behind Fable on front-end work. It
  tests thoroughly on its own, so say what verification a small task
  warrants. Effort: medium for most tasks, high for hard ones, and not
  above high — the reported gains past high are small at multiples of the
  cost. `none` is not accepted. Plus plans cap Astra usage; Pro plans
  don't.
- **Opus 5** — near-Fable coding and agentic performance at half Fable's
  token price ($5/$25), outside the Fable cap. With astra available, its
  job is mostly to spend Claude quota that Fable can't: reach for it when
  the Claude subscription has room and the Codex one doesn't, or as the
  Claude half of a cross-family review. Effort: medium for most tasks,
  high for especially difficult ones; above high, gains are likely small
  or negative — on scope-penalizing evals (FrontierCode) it peaks at
  medium because at higher effort it tends to do more than the task asked.
  Verifies its own work unprompted: if
  verification isn't wanted, say so explicitly, and if there's a preferred
  method (run the tests, build, a specific check), name it — otherwise it
  may pick the wrong one.
- **gpt-5.6-terra** — available, but astra at low or medium is likely the
  better choice.
- **gpt-5.6-luna** — truly simple, high-volume mechanical work: reading
  piles of documents, extraction, anything where judgement barely matters.
  Cheaper than Haiku, likely faster, and more capable.
- **Sonnet 5** — very high input-token-volume tasks where judgement still
  matters; the cheapest capable Claude.
- **Haiku** — almost never the right call; it's outdated and Sonnet is
  cheap enough.

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
edge over the GPT or Claude models here. Give it clear stop conditions and
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
shipped agents instead — `gpt-6-astra(low)`, `gpt-6-astra(medium)`,
`gpt-6-astra(high)`, `gpt-5.6-terra(high)`, `gpt-5.6-luna(high)` — or, for
any other model/effort combination, Workflow's
`agent(prompt, {model: 'gpt-6-astra', effort: 'low'})` — likewise
`{model: 'gpt-5.6-sol', effort: 'medium'}`, or
`{model: 'grok-4.6', effort: 'high'}` when the Grok family is configured.

For Claude models, include the `[1m]` suffix — `fable[1m]`, `sonnet[1m]`,
`opus[1m]` — in agent definitions and Workflow `model` params, or omit
`model` to inherit the parent's. It is harmless when the model already has
its full window, and the difference between 1M and 200K when it doesn't.
