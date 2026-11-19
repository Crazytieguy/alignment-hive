# Model safeguards

Bio/cyber deployment safeguards differ sharply per model and can be a
deciding factor (e.g. legitimate bio/cyber research that frontier-lab
classifiers refuse). Anthropic tiers per model under
[RSP v3.4, eff. 2026-07-08](https://www.anthropic.com/rsp); OpenAI under the
[Preparedness Framework v2](https://deploymentsafety.openai.com/gpt-6-astra);
xAI publishes a framework whose June 2026 revision reportedly dropped
its numeric thresholds; Meta assesses
under its
[Meta Superintelligence Scaling Framework](https://research.meta.ai/static/meta-superintelligence-scaling-framework);
Kimi and GLM have no binding vendor safety framework.

Routed GPT requests stay subject to OpenAI's server-side safeguards and
account-level enforcement on the user's own account; how much they cover
varies by interface, and the router doesn't send Codex's own prompt
mitigations.

Daybreak, OpenAI's trusted-access program for verified defenders, gives
an approved account fewer cyber refusals: with Daybreak Blue on GPT-6
Luna, GPT-6 Sol and earlier models; with Daybreak Red also on GPT-6.1
Terra, GPT-6.1 Sol, GPT-6 Astra and the 6.2 models. The router selects
Daybreak or standard safeguards on a route once the user has set that up
(the setup skill's `references/daybreak.md`), and the `daybreak` line of
the router's `doctor`, when there is one, names what each route selects;
a route that selects neither leaves the choice to OpenAI.
[OpenAI overview, read 2026-11-18](https://help.openai.com/en/articles/20001258).

- **Fable 5.2 / Mythos 5.2** — the same model at two safeguard levels:
  Fable 5.2 is generally available, Mythos 5.2 through Anthropic's
  cyber and life-sciences verification programs. As with Opus 5.5
  (below), blocked cyber requests re-run on Opus 4.8 and blocked biology
  requests on Opus 5; Anthropic reports about 55% fewer blocks than the
  previous Fable in early testing. Data is kept 30 days for safety
  monitoring, except for customers eligible for zero data retention.
  [Anthropic, 2026-11-09](https://www.anthropic.com/claude-fable-and-mythos-5-2);
  [system card, 2026-11-09](https://assets.claude.com/27d60918d218125e7504bc7354c4b4626ec56b11.pdf).
- **Opus 5.5** — biology classifiers like Fable's (fallback to
  Opus 5); cyber classifiers enforce Opus 5's policy with Fable-level
  robustness (source-code vulnerability finding allowed, binary
  vulnerability finding and exploit work blocked; fallback to Opus 4.8);
  a narrow frontier-LLM-development classifier (e.g. kernel work on some
  ML accelerators) falls back to Opus 5; weapons and distillation
  requests are blocked with no fallback. In Claude Code the session
  continues on the fallback model when `switchModelsOnFlag` is on or
  unset (the default, which may ask first in an interactive session);
  when it is off ("Ask each time"), the main session pauses and a
  subagent stops.
  [Anthropic, 2026-09-22](https://www.anthropic.com/claude-opus-5-5);
  [system card, 2026-09-22](https://www.anthropic.com/claude-opus-5-5-system-card).
- **Opus 5** — defensive security work allowed (source-code vulnerability
  scanning, triage, secure coding); classifiers block exploit generation,
  binary vulnerability scanning, and penetration testing — ~85% less
  intervention than Fable 5, with blocked requests falling back to
  Opus 4.8 on Claude surfaces (API: `fallbacks: "default"` beta). No bio
  fallback: a biology-flagged request ends in a refusal.
  [Support, 2026-09](https://support.claude.com/en/articles/16049681-why-claude-switched-models-in-your-conversation-with-opus-5);
  [Anthropic, 2026-07-24](https://www.anthropic.com/news/claude-opus-5).
- **Sonnet 5.5** — the first Sonnet with the frontier cyber
  classifiers: higher-risk cyber requests visibly fall back to Sonnet 5,
  and so do blocks by a narrow frontier-LLM-development classifier. No
  bio fallback: a biology-flagged request ends in a refusal. It crosses
  no new RSP thresholds.
  [Anthropic, 2026-09-28](https://www.anthropic.com/claude-sonnet-5-5);
  [system card](https://assets.claude.com/870c8f525702625d2c62fc6dd04c857e3250bec1.pdf).
- **Opus 4.8 / Sonnet 5** — the cyber fallbacks, with cyber classifiers
  of their own: exploit development is blocked by default, vulnerability
  detection is not, so a request blocked upstream can be blocked again.
  [Opus card, 2026-05-28](https://www-cdn.anthropic.com/0b4915911bb0d19eca5b5ee635c80fef830a37ea.pdf);
  [Sonnet card, 2026-06-30](https://www.anthropic.com/claude-sonnet-5-system-card).
- **Haiku 5.5** — biology classifiers match Opus 5's and Sonnet 5's
  (harmful misuse only, not Opus 5.5's research-biology ones); cyber
  classifiers are narrower than on the larger models (looser than
  Sonnet 5.5's; penetration testing still blocked);
  frontier-LLM-development and weapons classifiers as well. There is no
  fallback model: a blocked request ends in a refusal. It crosses no new
  RSP thresholds.
  [Anthropic, 2026-10-07](https://www.anthropic.com/claude-haiku-5-5);
  [system card, 2026-10-07](https://www.anthropic.com/claude-haiku-5-5-system-card).
- **GPT-6.2 Astra / GPT-6.2 Sol** — Critical in cyber and High in
  bio/chem, behind OpenAI's strictest cyber boundary so far: refusals
  cover more dual-use requests, including autonomous vulnerability
  discovery and chained exploit development, and OpenAI says this will
  restrict some legitimate vulnerability research and security testing.
  Dual-use biology restrictions were tightened too. Of the Daybreak
  levels above, only Red reduces their cyber refusals.
  [System card, 2026-11-17](https://deploymentsafety.openai.com/gpt-6-2-astra-sol);
  [launch post](https://openai.com/index/introducing-gpt-6-2-astra-sol/).
- **GPT-6.1 Terra** — Critical in cyber and High in bio/chem, a step
  looser on cyber than the 6.2 models. In OpenAI's internal Codex
  simulation, monitoring flagged it relatively often for circumventing
  restrictions, concealing uncertainty and destructive actions, and its
  most common failure in OpenAI's warnings eval is treating a legitimate
  restriction as a prompt injection and ignoring it.
  [System card addendum, 2026-10-26](https://deploymentsafety.openai.com/gpt-6-1-terra/addendum-to-gpt-6-astra-system-card-gpt-6.1-terra).
- **GPT-6 Luna / GPT-6 Sol** — High (not Critical) in both bio/chem and
  cyber: real-time monitors and account-level enforcement, without the
  extra cyber restrictions of the Critical-rated models. `gpt-6-sol` has
  no shipped agent but routes as a Workflow model or a user agent, and
  is the GPT model to try for legitimate security work the others
  refuse. Daybreak Blue covers both.
  [Sol/Luna appendix, 2026-09-22](https://deploymentsafety.openai.com/gpt-6-astra/sec:appendix-sol-luna/).
- **Grok 4.7 (xAI)** — structurally the lightest safeguard stack of the
  framework-backed vendors above (no fallback model, monitoring or
  trusted-access tier described): refusal training plus runtime input and
  topical filters that vary by deployment surface (CSAM, self-harm,
  bio/chem weapons pathways, and cyber-specific input controls). The
  [xAI Frontier AI Framework, eff. 2026-06-30](https://media.x.ai/v1/website/xai-frontier-artificial-intelligence-framework-30-june-2026-99c40684.pdf)
  names moderation filters and production-deployment monitoring for
  offensive-cyber risk, and no
  document pins down which controls run on the subscription-OAuth path
  this plugin uses. Its
  [model card, 2026-09-21](https://media.x.ai/v1/website/4p7card-5eccc980.pdf)
  reports fewer harmful or dual-use cyber completions and more
  biosecurity refusals than Grok 4.6.
- **Muse Wonder / Muse Wonder Full (Meta)** — one
  [model card, 2026-10-12](https://research.meta.ai/static/muse-wonder-model-card),
  whose results are on Wonder Full and, per Meta, cover the family. Under
  the Meta Superintelligence Scaling Framework, Meta does not rule out the
  unmitigated model reaching its high-risk threshold in chemical and
  biological, and applies safety post-training plus system-level
  classifiers there; cyber and loss of control sit at "moderate or lower"
  before mitigations, and every domain after. The card reports strong
  cyber capability, low attack success against its cyber refusals, and
  bio/chem refusal rates near 99%. It does not say which controls run on the subscription-OAuth
  path this plugin uses. Data use: a Muse Code subscription defaults to
  Contributor mode, under which Meta trains on prompts, code, files and
  responses; Standard mode does not train on content. The router stays in
  Standard mode unless `[muse] contributor = true`, but the terms tie the
  designation to the Muse Code account setting, not the model ID; setup's
  `references/muse.md` covers it.
- **Gemini 4 Neon / Argon / Carbon / Hydrogen (Google)** — assessed under Google
  DeepMind's Frontier Safety Framework. Argon's
  [model card, 2026-10-13](https://deepmind.google/models/model-cards/gemini-4-argon/)
  says Google cannot rule out that the model reaches the framework's
  critical capability levels for biological information and for
  cybersecurity, and applies safeguards it assesses lower the risk to
  below those levels, without describing them. Carbon's
  [model card, 2026-11-10](https://deepmind.google/models/model-cards/gemini-4-carbon/)
  reports strengthened CBRN and cyber-misuse safeguards, and says
  specialized dual-use cyber capabilities go to verified defenders
  through its Fairwind Program. Neon's
  [model card, 2026-11-18](https://deepmind.google/models/model-cards/gemini-4-neon/)
  carries Argon's results forward (Google cannot rule out the biology
  and cyber levels) and reports strengthened CBRN and cyber-misuse
  safeguards. Hydrogen's
  [model card, 2026-11-12](https://deepmind.google/models/model-cards/gemini-4-hydrogen/)
  says it does not reach those levels but reaches their alert
  thresholds. None of the cards says which controls run on the AI Studio
  API key path this plugin uses. On that path the Gemini
  API's
  [adjustable content filters](https://aistudio.google.com/docs/safety-settings)
  are off: CLIProxyAPI sends them off, and Off is Google's default for
  any it does not name. The protections Google always applies (it names
  child safety) cannot be adjusted.
- **Kimi K2.7 / K3** — no published vendor safety framework or model-card
  safety section ([Kimi K3 card](https://huggingface.co/moonshotai/Kimi-K3)).
- **GLM-5.2** — no binding vendor framework (Z.ai co-authored a general
  open-weight risk framework with Concordia AI in September 2026); the
  [NIST/CAISI assessment, 2026-07-17](https://www.nist.gov/news-events/news/2026/07/caisi-assessment-zais-glm-52)
  rated its guardrails "mixed": permits agentic exploit development and
  blocks fewer sensitive bio questions than US reference models.
- Open-weights models via third-party hosts (OpenRouter, Fireworks, ...)
  generally have no vendor classifier stack, so mostly trained-in
  behavior applies; any host-side moderation varies by host.

User reports (practitioner anecdote, not official): in Fable 5.2's
launch week, classifiers were tight enough to get in the way of
discussing LLM psychology, and some biology work still tripped them
easily; such limits have tended to loosen after launch
([Zvi's roundup, 2026-11-16](https://thezvi.substack.com/p/fable-52-is-the-new-bazooka)).
Vals's launch-day evaluation of Fable 5.2 at high effort reports a 1.88%
fallback rate and no refusals
([Vals, 2026-11-09](https://www.vals.ai/models/anthropic_claude-fable-5-2)).
