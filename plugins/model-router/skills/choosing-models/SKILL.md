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
subscriptions are more generous per dollar. The Codex allowance also
stretches much further on the smaller GPT-6 models: a Plus plan gets
roughly three times as many Sol messages as Astra messages, and about
seventy times as many Luna ones. Within the Claude family, Fable use is
capped at a share of the weekly limit (currently up to ~50%); the other
Claude models draw on the whole limit.

## Character differences

The Claude constitution treats the model as an entity expected to infer the
intent behind a request: Claude models (Fable especially) fill in unstated
requirements, notice contradictions and surface them, and take some liberty
interpreting what you meant. The OpenAI model spec produces a more literal
temperament: GPT models execute instructions as written and make fewer
mechanical mistakes doing so, so a fully specified task can go better on a
GPT model than on Fable.

What "literal" looks like differs by model. OpenAI describes GPT-6 Astra
as stopping to ask when an answer could change the result and handing
back a first implementation for review; users confirm the early stops,
and also report it over-engineering or going past the ask on long runs.
Sol and Luna share its training methods, with no reports yet on how
their temperament compares. GPT-5.6, by contrast, tended to over-persist
— working around constraints to finish rather than escalating. The same
prompt hygiene serves every one of them: state the desired outcome, the
constraints, what counts as done, and whether being blocked should be
reported back or worked around.

Claude and GPT models have de-correlated strengths and weaknesses: the
mistakes one family makes, the other tends to catch. For best results, have
a Claude model review GPT work and vice versa.

Claude models are much harder to prompt-inject than GPT models, and the
gap is between families rather than within them: on the Gray Swan
indirect-injection benchmark Opus 5.5 and Fable 5.1 tie near 1% attack
success over 15 attempts, GPT-6 Astra sits at 8.5% (down from 27% for
GPT-5.6), and OpenAI reports Sol and Luna improved over GPT-5.6 without
publishing a comparable number. A Claude session that a safety
classifier drops to Opus 4.8 loses much of that robustness. Claude Code's
auto mode adds its own classifier pass over tool calls for whichever
model is running, cutting the risk below these numbers for all of them.
Relevant when a task involves browsing untrusted websites or processing
untrusted content.

## Model notes

- **Opus 5.5** — the default for most work, as the main agent and for
  delegated implementation. Anthropic's benchmarks put it ahead of Fable
  5.1 on most coding and knowledge work at 40% of Fable's token price
  ($4/$20), though Anthropic says the real-world gap is narrower than the
  scores suggest. Artificial Analysis ranks it first, it draws on the
  general Claude limit rather than the Fable cap, and hands-on reports
  are still thin. Early reports put it ahead of Fable on writing, including
  copy that persists (prompts, skills, docs). Effort: medium (its
  default) for most tasks. At xhigh it thinks far more per turn, and on scope-penalizing evals
  (FrontierCode) it peaks at medium. Never use max: it outspends Fable in
  tokens there. Its system card flags overstating what it checked and
  asserting unverified inferences as fact, and it is weaker than Fable on
  open-ended research; on consequential work, have both Fable and Astra
  review it. In unattended runs it can end its turn on a progress report,
  so say what counts as done.
- **Fable** — review, judgement- and taste-heavy work such as design and
  front-end, and open-ended brainstorming and discussion. No measured edge
  over Opus 5.5 backs this; it is a working preference, not a benchmark
  result. Also a good second attempt when another model failed, and best
  for simple, clean code and for simplifying existing code. Effort: medium
  for most tasks. At high and above it does more on its own — extra
  verification, proactive edits — and token use climbs with it. Mind the
  Fable usage cap.
- **gpt-6-astra** — a reviewer alongside Fable, and the pick for
  ambitious, long-running build projects. Priced like Fable ($10/$50) but
  reported to spend a fraction of the tokens, and it bills Codex. Its
  clearest gains over other models are computer use, long autonomous runs,
  and research-level math, all confirmed by users (computer use from
  inside Claude Code as well); it is strong at 3D work too, though Opus
  5.5 may be as good there. On coding benchmarks it trades places with
  Opus 5.5, and reports on its front-end work are mixed. It tests
  thoroughly on its own, so say what verification a small task warrants.
  Effort: medium for most tasks, high for hard ones, and not above high —
  the reported gains past high are small at multiples of the cost. `none`
  is not accepted. Plus plans cap Astra usage; Pro plans don't.
- **gpt-6-sol** — cost-efficient work on the Codex subscription,
  high-volume work included, where frontier judgement isn't needed. A
  fifth of Astra's token price; OpenAI places it close behind Astra,
  while independent indexes put it level with GPT-5.6 overall. Define
  what done looks like. Effort: medium for
  most tasks, high for hard ones.
- **gpt-6-luna** — high-volume work that doesn't need frontier
  intelligence: reading piles of documents, extraction, triage,
  mechanical transforms. A twentieth of Sol's token price. Effort: high,
  OpenAI's suggested starting point.
- **Sonnet 5, Haiku 4.5** — obsolete: Opus 5.5 and the smaller GPT-6
  models beat them on cost-efficiency. Anthropic says Sonnet and Haiku 5.5
  follow in the coming weeks.

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
pool, preserving both Claude and Codex usage. Use grok-4.7 (legacy
grok-4.6 and grok-4.5 routes exist for agents that predate it) — a modest
step over 4.6 that spends about twice its tokens per task, with no
measured edge over the GPT or Claude models here. Give it clear stop conditions and
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
`gpt-6-astra(high)`, `gpt-6-sol(medium)`, `gpt-6-sol(high)`,
`gpt-6-luna(high)` — or, for any other model/effort combination,
Workflow's `agent(prompt, {model: 'gpt-6-luna', effort: 'medium'})`. With the
Grok family configured, `{model: 'grok-4.7', effort: 'high'}` works the
same way.

For Claude models, include the `[1m]` suffix — `fable[1m]`, `sonnet[1m]`,
`opus[1m]` — in agent definitions and Workflow `model` params, or omit
`model` to inherit the parent's. It is harmless when the model already has
its full window, and the difference between 1M and 200K when it doesn't.
