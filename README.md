# alignment-hive

Claude Code infrastructure for the AI safety research community, starting with MATS.

## What This Is

Shared knowledge and tooling for AI agents working with alignment researchers:

- **Plugin/skills marketplace** - Curated plugins with basic stuff like scholar handbook
- **Memory system** - Learning from sessions, optimized for simplicity and minimal intrusiveness
- **Common workflow skills** - Connect to compute, write SRPs, prepare lightning talks, etc.

## Current Plan

**Priority**: Technical infrastructure first, then content.

### Immediate Next Steps
1. Read [Claude Code Skills Training](https://huggingface.co/blog/sionic-ai/claude-code-skills-training) for memory system design
2. Set up memory system with single "ask permission to submit" tier
3. Test locally before setting up GitHub Action for processing
4. Create initial plugin with basic skills

### Memory System Design Notes
- Retrieval less important than storage (can improve later)
- Storage should be "yankable" (removable on request)
- Structure: single monolithic skill, sessions have labels/names/dates/descriptions
- Index lists all sessions in greppable format

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

Scholars can open PRs (not push to main). QC process TBD.
