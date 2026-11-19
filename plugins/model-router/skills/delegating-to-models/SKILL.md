---
name: delegating-to-models
description: This skill should be read before delegating work to subagents or Workflow agents. Covers picking a model from the Claude, GPT, and optionally Grok, Muse and Gemini families (strengths, cost dynamics), setting the effort level, prompt hygiene per family, combining independent runs, and the mechanics of routing to non-Claude models through model-router.
---

# Delegating to models

model-router makes GPT models available as native Claude Code subagents
alongside Claude models. This skill gives high-level strengths, weaknesses,
cost dynamics and effort guidance; weigh them against the task at hand
rather than following rules mechanically.

## Cost dynamics

Compare models by cost per task, not price per token: tokenizers differ,
and models spend very different numbers of tokens on the same work.
Artificial Analysis's index (independent, November 2026) gives a rough
guide: score, then the API cost of running the whole index, cache reads
included. Fable, Opus and Sonnet results include their safety
fallbacks (requests a classifier moves to an older model); Haiku's leave
out its 5x rate past 100k tokens (see its note).

| model | medium | high | xhigh |
|---|---|---|---|
| Fable 5.2 | 56 / $3,360 | 62 / $7,106 | 62 / $11,742 |
| Opus 5.5 | 48 / $2,031 | 52 / $2,716 | 57 / $4,766 |
| Sonnet 5.5 | 31 / $679 | 42 / $1,268 | 50 / $2,903 |
| Haiku 5.5 | 25 / $73 | 30 / $121 | 35 / $193 |
| gpt-6.2-astra | 57 / $1,457 | 60 / $2,007 | 62 / $2,484 |
| gpt-6.2-sol | 53 / $405 | 55 / $553 | 56 / $698 |
| gpt-6.1-terra | 38 / $190 | 42 / $267 | 45 / $400 |
| gpt-6-luna | 18 / $40 | 21 / $63 | 23 / $86 |

On agentic coding in each lab's own harness (Artificial Analysis's
Coding Agent Index), the best Fable 5.2 and Opus 5.5 runs score about
58, against about 53 for 6.2 Astra and 50 for 6.2 Sol.

Subscriptions change the picture. Claude plans buy far more
API-equivalent usage per dollar than ChatGPT plans: community
measurements collected by Real API Pricing (November 2026, rough, taken
before 6.2, assuming the plan is used to its limit all month) put a
month of Opus usage at about 64–77x the plan's price on Claude Pro and
Max, against about 14x for Sol on ChatGPT Plus and an estimated 27x on
Pro (from OpenAI's stated allowance, not measured). Sol's lower cost per task still comes out
ahead: at Artificial Analysis's medium-effort cost per task, the $100
ChatGPT Pro plan runs roughly twice as many Sol tasks as the $100 Claude
Max plan runs Opus tasks, and at $20 (Plus against Pro) about 1.3x as
many. GPT delegation draws on the ChatGPT plan's Codex
usage and leaves Claude usage untouched. An install on an OpenAI API
key pays per token instead. Fable use is capped at a share of the weekly Claude limit
(currently up to ~50%), and per token it draws on that limit about three
to five times what Opus does (user measurements, not confirmed by
Anthropic), so Fable work costs several times Opus work on a
subscription too.

## Character differences

The Claude constitution treats the model as an entity expected to infer the
intent behind a request: Claude models (Fable especially) fill in unstated
requirements, notice contradictions and surface them, and take some liberty
interpreting what you meant. The OpenAI model spec produces a more literal
temperament: GPT models execute instructions as written and make fewer
mechanical mistakes doing so.

Whatever the model, the delegation prompt should state:

- the outcome and the constraints;
- what counts as done, including which checks to run;
- whether being blocked should be reported back or worked around.

The 6.2 models may stop to suggest a next step and wait for
confirmation when intent is unclear. In OpenAI's targeted tests, 6.2
Astra left a known blocker out of its report about twice as often as
its predecessor, and a failed tool about three times as often. Codex's
own instructions counter this, but routed requests don't carry them, so
a prompt for a general-purpose 6.2 agent should also:

- say that nobody can answer questions mid-run: proceed on stated,
  reversible assumptions and put open questions in the final report;
- ask for a self-contained report listing what blocked it or failed,
  decisions made without guidance, and known flaws;
- say not to add files, tests, docs or config changes nobody asked for;
- leave out instructions to be brief: the 6.2 models already are, and
  OpenAI suspects its own push for concision may be behind the omitted
  blockers.

Claude and GPT models have decorrelated strengths and weaknesses: the
mistakes one family makes, the other tends to catch. For review of plans
or work authored by Claude, run GPT-6.2 Sol as the minimum (the shipped
`model-router:adversarial-code-reviewer` and `model-router:plan-reviewer`
agents run on it), then add reviewers on other models, the authoring
model included. Each added model makes the review more thorough, but reviewers
also propose changes beyond the task, so weigh their findings against
scope.

On the Gray Swan indirect-injection benchmark the larger Claude models
have been the hardest to prompt-inject: Opus 5.5 near 1% attack success
over 15 attempts, Sonnet 5.5 3.4%, Fable 5.2 4.1% and Haiku 5.5 7.1%. A
Claude session that a safety classifier drops to Opus 4.8 loses much of
that robustness; Anthropic attributes most of Fable 5.2's higher rate to
such fallbacks. Where the 6.2 GPT models sit is uncertain: OpenAI
reports large gains over earlier GPT models against its own attacker, but there
is no comparable independent result yet. Claude Code's auto mode adds its
own classifier pass over tool calls for whichever model is running, a
layer these benchmark rates don't measure. Relevant when a
task involves browsing untrusted websites or processing untrusted
content.

## Effort

Anthropic's [Spending your effort](https://claude.dev/blog/spending-your-effort/)
(Sep 2026) measured Claude models only; GPT and other models may respond
differently. Effort mostly sets how much verification and independent
judgement the model applies. Higher effort pays off most on tasks with
hidden edge cases (security, hardware, bug fixes in existing code,
numerical work), where the model reproduces before fixing, writes
adversarial tests and checks against reference implementations. It does
not fix a wrong reading of the task; a clearer spec does, and a detailed
spec also narrows the gap between levels. Unattended runs gain more from
higher effort than ones where a user can answer questions. The post's
guide: low for quick drafts to iterate on, medium for most feature work,
high where verification matters. Treat xhigh as the ceiling: on
Artificial Analysis's index, max adds between half a point and six
points over xhigh for 1.6–2.7x the cost on every model in the Cost
dynamics table, which shows what each step costs per model. The 6.2
models reject `none`.

## Model notes

- **Opus 5.5** — the default for most work, as the main agent and for
  delegated implementation, and ahead of Fable on writing, including
  copy that persists (prompts, skills, docs). It is less likely than
  Fable to make significant judgement calls on its own. Effort: medium
  by default, xhigh for hard ones, where it scores well above high for
  under twice the cost.
- **Fable 5.2** — most useful when you want to be hands-off and let the
  model make significant judgement calls on its own, and on hard, long
  or complex tasks; on ordinary work users mostly can't tell it from
  Opus. A good advisor to Opus, especially to point out where an
  approach is overly cautious or complicated, and a good second attempt
  when another model failed. Best for simple, clean code and for
  simplifying existing code: tell it the code is its own to restructure,
  or it tends to build on the existing structure. Reports are mixed: it
  can stop with doable work left or conclude on thin evidence. Effort:
  medium by default, high for hard ones; at high and above it does more on
  its own, and high costs about twice medium. Mind the Fable usage cap.
- **gpt-6.2-astra** — the strongest GPT model, within a few points of
  Fable 5.2 on Artificial Analysis's index for under half of Fable's
  cost there at medium and a fifth at xhigh. Its main role here is an
  extra decorrelated reviewer on consequential work. Launch-day reports (a few reviewers; not measured
  here) are mixed: praise for code review, 3D and computer use;
  complaints about long builds that stop early or loop, and write-ups
  that are hard to follow. Effort: medium by default, high or xhigh for
  hard ones.
- **gpt-6.2-sol** — the everyday GPT model, also good for self-contained
  non-coding work such as audits. At medium it scores above Opus 5.5 at
  high on Artificial Analysis's index for under a sixth of the cost.
  Effort: medium by default, high where verification matters or the
  task is long.
- **gpt-6.1-terra** — a solid model for simple, repetitive agentic work
  at volume. On Artificial Analysis's index, Sol at medium outscores
  Terra at xhigh for about the same cost, so Terra pays off where its
  lower token price counts and a lower effort on Sol wouldn't save much:
  work that reads a fairly fixed amount of input, such as stepping
  through every screen of an app to take screenshots, or running a large
  eval. Effort: medium by default.
- **gpt-6-luna** — the simplest, highest-volume work: reading piles of
  documents, extraction, triage and mechanical transforms. Effort: high
  by default.
- **Sonnet 5.5** — a third to two thirds of Opus 5.5's cost at the same
  effort, for a clearly lower score; Opus at high outscores Sonnet at
  xhigh for less. Effort: medium or high; the other efforts are rarely
  optimal.
- **Haiku 5.5** — the Claude counterpart to gpt-6-luna. Artificial
  Analysis's index puts it somewhat ahead of Luna at every effort level
  for about twice the cost, but at Haiku's flat rate: once a prompt
  passes 100k tokens, the whole request is billed at 5x (input, output
  and cache reads), and Claude Code by default doesn't compact anywhere
  near that point, so on long tasks it costs far more than the table shows. It draws on the Claude
  limit, so when Opus or Fable carry most of the work, Luna usually
  spends the less strained subscription. For truly high-volume use, run
  the blind comparison below against Luna. Anthropic's prompting guide
  says that at low and medium effort it sometimes reports a change as
  done without running a check.

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
pairs gain the most from the decorrelated strengths above; have the
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
pool, preserving both Claude and Codex usage. Use grok-4.7; it has no
measured edge over the GPT or Claude models here. Effort: xAI defaults it to high, and
subscription billing makes latency its only cost; it also accepts xhigh.
Its web search returns fewer, title-less results than the Claude and GPT
backends — prefer another family for search-heavy tasks.

## Muse models (optional)

Like Grok, use Meta's Muse models on the user's request or stated
preference, not autonomously. Likely reasons to ask: how the models
write, a decorrelated opinion from another model family, and work that
the other families' safeguards get in the way of, which may be worth a
try here (`references/safeguards.md`; Meta's model card claims strong
cyber safeguards, so no promise). The subscription quota goes too
quickly to count as a spare billing pool.
Two routes: `muse-wonder-1.0` (Muse Wonder) and `muse-wonder-full-1.0`
(Muse Wonder Full). Meta serves Full only on its Power and Maximum
plans, so setup creates its agent only when the login's plan allows it.
Early reports (launch week, October 2026; not measured here) put Full
ahead of Wonder on harder tasks, near frontier level on agentic coding
and following instructions, and weak at UI work; some users like how
Full writes (direct, concise; long-form got mixed marks), which
suggests human-facing text, or Full as a main agent that delegates.

Short Claude Code tasks on Wonder mostly went well. When asking for a
plan first, say to continue into the work.
Quota: the subscription has 5-hour and weekly windows; one near-1M
request took about 40% of a High plan's 5-hour window. One small
outside test put Full at 2-3x Wonder's quota per task. Effort: every
level passes through unchanged; Muse Code defaults to high and advises
the lowest level that gives acceptable results.

## Gemini models (optional)

Like Grok and Muse, use Google's Gemini models on the user's request or
stated preference, not autonomously. The usual reason is writing, where
evidence is thin: on Arena's human votes (November 18, 2026), Argon,
Carbon and Neon sit within five points of Opus 5.5 on creative writing,
and Hydrogen 25 behind. Default to the `gemini-4-neon` agent; use
`gemini-4-argon`, `gemini-4-carbon` or the cheaper `gemini-4-hydrogen`
when the user names one. A user on Google's free tier has only Hydrogen,
without web search, and outside the EEA, Switzerland and the UK Google
may use what it receives to improve its products: send it nothing
sensitive. Effort tops out at high on Argon and Carbon, xhigh on Neon
and Hydrogen. Paid calls bill the user's AI Studio key; on a 429, wait
as long as Google's message says, or run fewer large-context Gemini
agents at once. If model-router refuses a Hydrogen request, relay the
reason and fix from its message; for a project without billing,
`/model-router:setup` also explains the free-tier option.

## Open-weights models

Open-weights models (Kimi, GLM, ...) are optional; `model-router:setup`
configures the routes and creates their agents. Use them only when the
user explicitly asks for them; there is no strengths/weaknesses guidance
for them yet.

## Mechanics

The Agent tool's `model` parameter only accepts Claude models. For GPT,
use the shipped agents, whose agent types carry the plugin's prefix:
`model-router:gpt-6.2-astra`, `model-router:gpt-6.2-sol`,
`model-router:gpt-6.1-terra`, `model-router:gpt-6-luna`. In Workflow, pass
the bare route as the model: `agent(prompt, {model: 'gpt-6.2-sol', effort:
'medium'})`. The Grok, Muse and Gemini families have per-user agents that
setup creates, named for the route without a prefix (`grok-4.7`,
`muse-wonder-1.0`, `gemini-4-neon`), and their routes work the same way
in Workflow.

Set the `effort` parameter for general-purpose and Workflow agents.
Forks run at this session's effort. Custom agents can set their own
default, and the shipped GPT agents do; override it only when the task
calls for more or less.

Each of the four GPT agents has a `-fast` twin on Codex's fast tier
(`model-router:gpt-6.2-sol-fast`, Workflow model `gpt-6.2-sol-fast`),
faster for more usage, as its description says. Use one only when speed
matters more than usage, such as a result the user is waiting on right
now.

On plans that have OpenAI's Ultrafast tier, the user may have added
`gpt-6.2-sol-ultrafast` and `gpt-6.2-astra-ultrafast` agents: up to 4.5x
faster for 6x the usage. Use one only when the user asks for Ultrafast.
