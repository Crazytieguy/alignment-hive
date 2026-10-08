---
name: delegating-to-models
description: This skill should be read before delegating work to subagents or Workflow agents. Covers picking a model from the Claude, GPT, and optionally Grok families (strengths, cost dynamics), setting the effort level, prompt hygiene per family, combining independent runs, and the mechanics of routing to non-Claude models through model-router.
---

# Delegating to models

model-router makes GPT models available as native Claude Code subagents
alongside Claude models. This skill gives high-level strengths, weaknesses,
cost dynamics and effort guidance; weigh them against the task at hand
rather than following rules mechanically.

## Cost dynamics

Token price and token *usage* are different axes; per-task cost is their
product, and the models below differ on both. Under subscriptions the
billing pool matters more than list price: GPT delegation bills the
separate Codex subscription and preserves Claude usage entirely, and OpenAI
subscriptions are more generous per dollar. The Codex allowance roughly
tracks token price, so it stretches much further on the smaller GPT
models. Within the Claude family, Fable use is
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
Sol and Luna share its training methods; how their temperament compares
is less documented. GPT-5.6, by contrast, tended to over-persist
— working around constraints to finish rather than escalating. The same
prompt hygiene serves every one of them: state the desired outcome, the
constraints, what counts as done, and whether being blocked should be
reported back or worked around.

Claude and GPT models have de-correlated strengths and weaknesses: the
mistakes one family makes, the other tends to catch. For best results, have
a Claude model review GPT work and vice versa.

The larger Claude models are much harder to prompt-inject than GPT
models: on the Gray Swan indirect-injection benchmark Opus 5.5 and Fable
5.1 tie near 1% attack success over 15 attempts, Sonnet 5.5 sits at
3.4%, Haiku 5.5 at 7.1%, GPT-6 Astra at 8.5% (down from 27% for
GPT-5.6), and OpenAI reports Sol and Luna improved over GPT-5.6 without
publishing a comparable number. A Claude session that a safety
classifier drops to Opus 4.8 loses much of that robustness. Claude Code's
auto mode adds its own classifier pass over tool calls for whichever
model is running, cutting the risk below these numbers for all of them.
Relevant when a task involves browsing untrusted websites or processing
untrusted content.

## Effort

Anthropic's [Spending your effort](https://claude.dev/blog/spending-your-effort/)
(Sep 2026) measured Claude models only (Opus 5.5, Fable 5.1); GPT and other
models may respond differently. Effort mostly sets how much verification
and independent judgement the model applies. Higher effort pays off most on
tasks with hidden edge cases (security, hardware, bug fixes in existing
code, numerical work), where the model reproduces before fixing, writes
adversarial tests and checks against reference implementations. It does
not fix a wrong reading of the task; a clearer spec does, and a detailed
spec also narrows the gap between levels. Unattended runs gain more from
higher effort than ones where a user can answer questions. The post's
guide: low for quick drafts to iterate on, medium for most feature work,
high where verification matters, max for fully autonomous hard problems
(the per-model notes below qualify this). Low to max cost about 3x the
tokens, and wall-clock time grows from minutes to over an hour.

## Model notes

- **Opus 5.5** — the default for most work, as the main agent and for
  delegated implementation. Anthropic's benchmarks put it ahead of Fable
  5.1 on most coding and knowledge work at 40% of Fable's token price
  ($4/$20), though Anthropic says the real-world gap is narrower than the
  scores suggest. Artificial Analysis ranks it first, it draws on the
  general Claude limit rather than the Fable cap, and user reports put
  it ahead of Fable on writing, including
  copy that persists (prompts, skills, docs). Effort: medium (its
  default) for most tasks. At xhigh it thinks far more per turn, and on scope-penalizing evals
  (FrontierCode) it peaks at medium. Never use max: it outspends Fable in
  tokens there. Its system card flags overstating what it checked and
  asserting unverified inferences as fact, and it is weaker than Fable on
  open-ended research; on consequential work, have both Fable and
  GPT-6.1 Sol review it. In unattended runs it can end its turn on a progress report,
  so say what counts as done.
- **Fable** — review, judgement- and taste-heavy work such as design and
  front-end, and open-ended brainstorming and discussion. No measured edge
  over Opus 5.5 backs this; it is a working preference, not a benchmark
  result. Also a good second attempt when another model failed, and best
  for simple, clean code and for simplifying existing code. Effort: medium
  for most tasks. At high and above it does more on its own — extra
  verification, proactive edits — and token use climbs with it. Mind the
  Fable usage cap.
- **gpt-6-astra** — the strongest GPT model, for hard tasks where 6.1 Sol
  falls short. Priced like Fable ($10/$50) but reported to spend a fraction of the tokens, and it bills Codex. Its
  clearest gains over other models are computer use, long autonomous runs,
  and research-level math, all confirmed by users (computer use from
  inside Claude Code as well); it is strong at 3D work too, though Opus
  5.5 may be as good there. On coding benchmarks it trades places with
  Opus 5.5, and reports on its front-end work are mixed. It tests
  thoroughly on its own, so say what verification a small task warrants.
  Effort: medium for most tasks, high for hard ones, and not above high —
  the reported gains past high are small at multiples of the cost. `none`
  is not accepted. Plus plans cap Astra usage; Pro plans don't.
- **gpt-6.1-sol** — a reviewer alongside Fable, and self-contained
  non-coding work such as audits; for coding, Claude models look like
  the better default. A fifth of Astra's token price ($2/$10). User
  reports suggest it reviews about as well as Astra, an impression rather
  than a measurement; OpenAI and Artificial Analysis both place it just
  behind Astra overall. Define what done looks like. Effort: medium or
  high by default, but the full range is usable.
- **gpt-6-luna** — high-volume work that doesn't need frontier
  intelligence: reading piles of documents, extraction, triage,
  mechanical transforms. A twentieth of Sol's token price. Effort: high,
  OpenAI's suggested starting point.
- **Sonnet 5.5** — half of Opus 5.5's token price ($2/$10, cache reads
  included), but Opus 5.5 uses fewer tokens per task, so the per-task
  gap is much smaller than the price gap. Effort: medium or high,
  the other efforts are rarely optimal.
- **Haiku 5.5** — the Claude counterpart to gpt-6-luna. It looks somewhat
  more capable than Luna (Artificial Analysis's index puts it ahead at
  every effort level) and somewhat more expensive in practice: both list
  at $0.10/$0.50, but Haiku 5.5 spends about two to three times Luna's
  tokens at the same effort, and token counts aren't directly comparable
  across the two tokenizers. Once a prompt passes 100k tokens, the whole
  request is billed at 5x (input, output and cache reads), and Claude
  Code doesn't compact anywhere near that point, so it is cheapest on
  short tasks. It draws on the Claude limit, so when Opus or Fable carry
  most of the work, Luna usually spends the less strained subscription. For truly high-volume use, benchmark the task
  against Luna with larger models such as Opus 5.5 and GPT-6.1 Sol as
  judges. Anthropic's prompting guide says that at low and medium effort
  it sometimes reports a change as done without running a check, so say
  what verification is expected.

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

The Agent tool's `model` parameter only accepts Claude models. For GPT,
use the shipped agents — `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6-luna` — or
Workflow's `agent(prompt, {model: 'gpt-6-luna', effort: 'medium'})`. With
the Grok family configured, `grok-4.7` agents and
`{model: 'grok-4.7', effort: 'high'}` work the same way.

Consider setting the Agent tool's `effort` parameter when delegating,
Claude models included. Without it, the GPT, Grok and open-weights agents
run at this session's effort level.
