# Model safeguards

Bio/cyber deployment safeguards differ sharply per model and can be a
deciding factor (e.g. legitimate bio/cyber research that frontier-lab
classifiers refuse). Anthropic tiers per model under
[RSP v3.4, eff. 2026-07-08](https://www.anthropic.com/rsp); OpenAI under the
[Preparedness Framework v2](https://deploymentsafety.openai.com/gpt-5-6);
Kimi and GLM publish no vendor safety framework.

- **Fable 5 / Mythos 5** — ASL-3 plus a Mythos-class classifier layer that
  reroutes cyber, bio/chem, and distillation queries to Opus 4.8 (<5% of
  sessions, user notified), tuned deliberately over-cautious.
  [Anthropic, 2026-06-09](https://www.anthropic.com/news/claude-fable-5-mythos-5);
  [redeploy update, 2026-07-01](https://www.anthropic.com/news/redeploying-fable-5).
- **Opus 4.8 / Sonnet 5** — same posture: ASL-3 ("equal to or stronger than
  historical ASL-3"), narrow CBRN classifiers only, no blocking cyber
  classifier.
  [Opus card, 2026-05-28](https://www-cdn.anthropic.com/0b4915911bb0d19eca5b5ee635c80fef830a37ea.pdf);
  [Sonnet card, 2026-06-30](https://www.anthropic.com/claude-sonnet-5-system-card).
- **Haiku 4.5** — ASL-2; lightest safeguards of the family.
  [Card, 2025-10](https://assets.anthropic.com/m/99128ddd009bdcb/original/Claude-Haiku-4-5-System-Card.pdf).
- **GPT-5.6 (sol/terra/luna, via Codex)** — "High" (not Critical) in both
  bio/chem and cyber: real-time monitors plus account-level enforcement.
  [System card, Jul 2026](https://deploymentsafety.openai.com/gpt-5-6).
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
run over-strict for legitimate work — benign DevOps sessions silently
downgraded to Opus, security-work refusals
([claude-code#74734](https://github.com/anthropics/claude-code/issues/74734);
[The Register, 2026-06-10](https://www.theregister.com/ai-and-ml/2026/06/10/anthropic-claude-fable-5-refuses-innocuous-prompts/5253754)).
GPT-5.6/Codex is reported less strict in practice, though not refusal-free:
in [one comparison](https://www.techtimes.com/articles/319808/20260707/gpt-56-sol-review-faster-coding-half-fable-5-cost-benchmark-problem.htm)
both Codex and Fable refused exploit-adjacent security fixes that Kimi K3
completed.
