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

Consider modern tooling where appropriate. Examples: `uv` for Python, `vite` and `pnpm` for JavaScript/TypeScript.

## Living Documentation

A useful pattern: treat README.md as a working document that evolves with the project, and add a CLAUDE.md with instructions to keep it updated.

## Claude Code Plugins

Browse available plugins at: https://github.com/anthropics/claude-plugins-official/tree/main/plugins

Install by adding to `.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "pyright-lsp@claude-plugins-official": true
  }
}
```

Some plugins by project type:
- **Python**: `pyright-lsp`
- **TypeScript/JavaScript**: `typescript-lsp`, `frontend-design`
- **Rust**: `rust-analyzer-lsp`
- **Agent development**: `agent-sdk-dev`
