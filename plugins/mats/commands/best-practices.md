---
description: Use when the user asks about "best practices", "how should I set up", "what's the right way to", "help me get started", "start a project", "set up my environment", "which plugins should I install", "how to configure Claude Code", "optimize for Claude", "make my project work better with Claude", "project structure", "what tools should I use", "improve my tooling", "improve my dev workflow", or mentions project architecture, tooling choices, or Claude Code configuration.
---

# Best Practices

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

Consider modern tooling where appropriate. Examples: `uv` for Python, `vite` and `bun` for JavaScript/TypeScript. Be proactive about including and proposing linters, typechecking, etc.

If a tool would be useful and isn't installed, ask if the user would like to install it.

## Living Documentation

A useful pattern: treat README.md as a working document that evolves with the project, and add a CLAUDE.md with instructions to keep it updated. Reference the README from CLAUDE.md using `@README.md` so Claude always has context on the project.

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

After installing plugins, tell the user to exit ("/exit") and continue the session with "claude -c".

## MATS / Alignment Researchers

**Always ask** if the user wants to install hive-mind. This is a key part of the alignment research workflow - don't skip it. hive-mind provides local memory retrieval, shared knowledge from other researchers' Claude sessions, and your sessions contribute back. Sharing requires an invite (MATS fellows: check your email).

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

Tell the user to run `/hive-mind:setup` to complete the hive-mind configuration, after they restart Claude Code.

## Completion

Once all recommendations have been either implemented or explicitly rejected by the user, write the current plugin version to `.claude/mats:best-practices-version`. Get the version from `!cat "${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json" | grep '"version"' | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/'`.
