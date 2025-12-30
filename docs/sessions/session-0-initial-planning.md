# Session 0: Initial Planning

**Date**: 2024-12-24

## Goal

Establish overall architecture and identify key questions for the memory system.

## Key Insight

Privacy/sanitization is more foundational than originally thought. The decision about whether to store raw JSONL in git (enabling reprocessing but risking leaks) vs a separate server (safer but more infrastructure) affects nearly everything else.

## Decisions Made

- Storage prioritized over retrieval (can improve retrieval later; losing data is permanent)
- Human review required for all submissions (quality control, privacy protection)
- Attribution required for yankability (users must be able to identify and remove their data)
- Derive labels from content, not user input (reduces friction; AI can extract this)

## Reference Material

- [Claude Code Skills Training blog post](https://huggingface.co/blog/sionic-ai/claude-code-skills-training) - Sionic AI's approach with /retrospective and /advise commands
- Existing project-setup plugin structure in this repo
- Claude Code hooks/skills/commands documentation
