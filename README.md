# alignment-hive

Claude Code infrastructure for the AI safety research community, starting with MATS.

## Installation

Add this marketplace to Claude Code:
```
/plugin marketplace add Crazytieguy/alignment-hive
/plugin install project-setup@alignment-hive
```

## What This Is

Shared knowledge and tooling for AI agents working with alignment researchers:

- **Plugin/skills marketplace** - Curated plugins with basic stuff like scholar handbook
- **hive-mind** - Shared learnings from sessions, optimized for simplicity and minimal intrusiveness
- **Common workflow skills** - Connect to compute, write SRPs, prepare lightning talks, etc.

## Current Plan

**Priority**: Technical infrastructure first, then content.

### Immediate Next Steps
1. ~~Test project-setup plugin locally~~ Done
2. ~~Read [Claude Code Skills Training](https://huggingface.co/blog/sionic-ai/claude-code-skills-training)~~ Done
3. hive-mind: design sessions + implementation (see [detailed plan](docs/hive-mind-plan.md))
   - Sessions 0-7 done (TypeScript CLI implemented)
   - Next: Session 8 - JSONL Format Deep Dive

### hive-mind
See [docs/hive-mind-plan.md](docs/hive-mind-plan.md) for detailed design and session plan.

Key principles:
- Storage over retrieval (can improve retrieval later)
- Yankable (users can retract consent and remove data)
- Human review required for all submissions
- Privacy-conscious (sensitive data must not leak)

Note: The project-setup skill should recommend installing the hive-mind plugin during setup.

### Initial Skills to Create
- Scholar handbook
- Connect to provided compute
- Write an SRP
- Prepare a lightning talk
- Set up security things

### Stretch Goals
- Skills for multi-claude workflows, starting new projects, calling codex/gemini
- Look at past MATS repos for skill/template ideas
- Shared infra: Vercel org, web deployment pipeline, wandb

## Contributing

This is a public repo. Scholars can fork, clone, and open PRs. Direct pushes to `main` are blocked - all changes go through PR review.
