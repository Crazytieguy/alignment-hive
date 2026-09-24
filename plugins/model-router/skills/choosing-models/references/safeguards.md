# Model safeguards

Bio/cyber deployment safeguards differ sharply per model and can be a
deciding factor (e.g. legitimate bio/cyber research that frontier-lab
classifiers refuse). Anthropic tiers per model under
[RSP v3.4, eff. 2026-07-08](https://www.anthropic.com/rsp); OpenAI under the
[Preparedness Framework v2](https://deploymentsafety.openai.com/gpt-6-astra);
xAI publishes a framework without quantitative thresholds; Kimi and GLM
publish no vendor safety framework.

- **Fable 5.1 / Mythos 5.1** — the same model at two safeguard levels:
  Fable 5.1 is generally available, Mythos 5.1 only through trusted-access
  programs for cyber and life-sciences work. Fable 5.1 runs a classifier
  layer that reroutes cyber, bio/chem, and distillation queries (the user
  is notified): blocked cyber tasks complete on Opus 4.8, blocked biology
  tasks on Opus 5. The classifiers are tuned to stay out of ordinary work,
  security-adjacent coding included; heavy safeguards remain on biology.
  Zero data retention is available to eligible customers.
  [Anthropic, 2026-09-01](https://www.anthropic.com/claude-fable-and-mythos-5-1);
  [system card, 2026-09-01](https://www-cdn.anthropic.com/0339e6a7c5c7b87f5c07798616dc32c215d14235/Claude%20Fable%205.1%20&%20Claude%20Mythos%205.1%20System%20Card.pdf).
  Fable 5 (still routable) has the earlier, deliberately over-cautious
  classifiers.
  [Anthropic, 2026-06-09](https://www.anthropic.com/news/claude-fable-5-mythos-5).
- **Opus 5.5** — biology classifiers match Fable 5.1's (fallback to
  Opus 5); cyber classifiers enforce Opus 5's policy with Fable-level
  robustness (source-code vulnerability finding allowed, binary
  vulnerability finding and exploit work blocked; fallback to Opus 4.8);
  a narrow frontier-LLM-development classifier (e.g. kernel work on some
  ML accelerators) falls back to Opus 5; weapons and distillation
  requests are blocked with no fallback. In Claude Code the session
  continues on the fallback model. Rated CB-1, below CB-2.
  [Anthropic, 2026-09-22](https://www.anthropic.com/claude-opus-5-5);
  [system card, 2026-09-22](https://www.anthropic.com/claude-opus-5-5-system-card).
- **Opus 5** — defensive security work allowed (source-code vulnerability
  scanning, triage, secure coding); classifiers block exploit generation,
  binary vulnerability scanning, and penetration testing — ~85% less
  intervention than Fable 5, with blocked requests falling back to
  Opus 4.8 on Claude surfaces (API: `fallbacks: "default"` beta). No bio
  fallback: biology/chem stays on Opus 5 under Opus-4.8-level safeguards.
  [Support, 2026-07](https://support.claude.com/en/articles/16049681-why-claude-switched-models-in-your-conversation-with-opus-5);
  [Anthropic, 2026-07-24](https://www.anthropic.com/news/claude-opus-5).
- **Opus 4.8 / Sonnet 5** — same posture: ASL-3 ("equal to or stronger than
  historical ASL-3"), narrow CBRN classifiers only, no blocking cyber
  classifier.
  [Opus card, 2026-05-28](https://www-cdn.anthropic.com/0b4915911bb0d19eca5b5ee635c80fef830a37ea.pdf);
  [Sonnet card, 2026-06-30](https://www.anthropic.com/claude-sonnet-5-system-card).
- **Haiku 4.5** — ASL-2; lightest safeguards of the family.
  [Card, 2025-10](https://assets.anthropic.com/m/99128ddd009bdcb/original/Claude-Haiku-4-5-System-Card.pdf).
- **GPT-6 Astra (via Codex)** — the first model at "Critical" cyber
  capability under the Preparedness Framework, and the strictest GPT
  deployment: the released model refuses proof-of-concept exploit
  writing (secure code review and patching are allowed), refuses cyber
  jailbreak prompts far more often than sol (91.5% vs 59%), and
  tool-using traffic is monitored for misalignment with account-level
  enforcement. It also refuses more on some bio evaluations, and users
  report refusals beyond security (reactions to fiction, election
  predictions). Elevated cyber access runs through the Daybreak
  trusted-access program for verified defenders.
  [System card, 2026-09-03](https://deploymentsafety.openai.com/gpt-6-astra);
  [Path to Astra](https://openai.com/index/path-to-astra/).
- **GPT-6 Sol / Luna and GPT-5.6 (via Codex)** — "High" (not Critical)
  in both bio/chem and cyber: real-time monitors plus account-level
  enforcement, without Astra's extra cyber restrictions. Sol and Luna
  refuse harmful requests about as often as their GPT-5.6 counterparts;
  their cyber jailbreak resistance sits between GPT-5.6 sol's and
  Astra's.
  [Sol/Luna appendix, 2026-09-22](https://deploymentsafety.openai.com/gpt-6-astra/sec:appendix-sol-luna/);
  [GPT-5.6 card, Jul 2026](https://deploymentsafety.openai.com/gpt-5-6).
- **Grok 4.7 (xAI)** — the lightest safeguard stack of the
  framework-backed vendors: refusal training plus runtime input and
  topical filters that vary by deployment surface (CSAM, self-harm,
  bio/chem weapons pathways, and cyber-specific input controls). The
  [xAI Frontier AI Framework, eff. 2026-06-30](https://media.x.ai/v1/website/xai-frontier-artificial-intelligence-framework-30-june-2026-99c40684.pdf)
  names moderation filters and production-deployment monitoring for
  offensive-cyber risk but contains no quantitative thresholds, and no
  document pins down which controls run on the subscription-OAuth path
  this plugin uses. Its
  [model card, 2026-09-21](https://media.x.ai/v1/website/4p7card-5eccc980.pdf)
  reports fewer harmful or dual-use cyber completions than 4.6 (3.3% vs
  5.9% on its HackerBench), more biosecurity refusals, and CBRN and
  general refusals in line with 4.6. The legacy grok-4.6 route has its own
  [card](https://media.x.ai/v1/website/card-4p6-4cd2dc57.pdf); grok-4.5
  shipped with none, so what is deployed on it is uncertain.
- **Kimi K2.7 / K3** — no published vendor safety framework or model-card
  safety section (checked 2026-07-21:
  [github.com/moonshotai/kimi-k2](https://github.com/moonshotai/kimi-k2)).
- **GLM-5.2** — no framework; the
  [NIST/CAISI assessment, 2026-07-17](https://www.nist.gov/news-events/news/2026/07/caisi-assessment-zais-glm-52)
  rated its guardrails "mixed": permits agentic exploit development and
  blocks fewer sensitive bio questions than US reference models.
- Open-weights models via third-party hosts (OpenRouter, Fireworks, ...) add
  no host-side classifier stack — only trained-in behavior applies.

User reports (practitioner anecdote, not official): Fable 5's classifiers
ran over-strict for legitimate work — benign DevOps sessions silently
downgraded to Opus, security-work refusals
([claude-code#74734](https://github.com/anthropics/claude-code/issues/74734);
[The Register, 2026-06-10](https://www.theregister.com/ai-and-ml/2026/06/10/anthropic-claude-fable-5-refuses-innocuous-prompts/5253754)).
First-week reports on Fable 5.1: the classifiers stay out of ordinary
work unless you are pushing it, occasional downgrades to Opus 4.8 still
happen, and they are less strict than Astra's
([Zvi's roundup, 2026-09-05](https://thezvi.substack.com/p/claude-mythos-51-and-fable-51-capabilities)).
GPT-5.6/Codex is reported less strict in practice, though not refusal-free:
in [one comparison](https://www.techtimes.com/articles/319808/20260707/gpt-56-sol-review-faster-coding-half-fable-5-cost-benchmark-problem.htm)
both Codex and Fable refused exploit-adjacent security fixes that Kimi K3
completed. Astra refuses more of this by design; gpt-6-sol, without Astra's extra
cyber restrictions, is the GPT model to try for such work.
