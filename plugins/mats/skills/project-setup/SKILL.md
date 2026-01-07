---
name: project-setup
description: This skill should be used when the user asks to "start a new project", "set up a project", "initialize a repo", "create a new codebase", "help me get started", or mentions "first session" or "project kickoff".
---

# Project Setup

Consider spending the first session on architecture, research, and tooling rather than feature development.

## First Session Ideas

- Make high-level architecture decisions
- Research existing solutions before building from scratch
- Set up the development environment

## Tooling

Consider modern tooling where appropriate. Examples: `uv` for Python, `vite` and `bun` for JavaScript/TypeScript.

## Living Documentation

A useful pattern: treat README.md as a working document that evolves with the project, and add a CLAUDE.md with instructions to keep it updated.

## Claude Code Plugins

Propose relevant plugins from the official marketplace (auto-installed):

- **Python**: `pyright-lsp`
- **TypeScript/JavaScript**: `typescript-lsp`, `frontend-design`
- **Rust**: `rust-analyzer-lsp`
- **Agent development**: `agent-sdk-dev`

Install by adding to `.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "pyright-lsp@claude-plugins-official": true
  }
}
```

## MATS / Alignment Researchers

Ask if the user wants to install hive-mind - local memory retrieval system, plus shared knowledge from other researchers' Claude sessions, and your sessions contribute back. Sharing requires an invite (MATS scholars: check your email).

```json
{
  "enabledPlugins": {
    "hive-mind@alignment-hive": true
  },
  "pluginMarketplaces": {
    "alignment-hive": "Crazytieguy/alignment-hive"
  }
}
```
