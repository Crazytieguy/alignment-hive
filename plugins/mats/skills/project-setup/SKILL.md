---
name: project-setup
description: This skill should be used when the user asks to "start a new project", "set up a project", "initialize a repo", "create a new codebase", "help me get started", mentions "first session" or "project kickoff", or asks to "set up an existing project" or "configure this project".
---

# Project Setup

This skill applies to both new and existing projects.

## New Projects

For new projects, consider spending the first session on architecture, research, and tooling rather than jumping into feature development:

- Make high-level architecture decisions
- Research existing solutions before building from scratch
- Set up the development environment

## Existing Projects

For existing projects, offer to help with architecture or codebase organization if the user wants, but don't push it. Focus on:

- Understanding the current structure
- Suggesting relevant plugins (see below)
- Helping with whatever task brought them here

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

Install by adding to `./.claude/settings.json` in the project root (not `~/.claude/settings.json` or `settings.local.json`):

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
