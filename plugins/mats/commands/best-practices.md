---
description: Use when the user asks about "best practices", "how should I set up", "what's the right way to", "help me get started", "start a project", "set up my environment", "which plugins should I install", "how to configure Claude Code", "optimize for Claude", "make my project work better with Claude", "project structure", "what tools should I use", "improve my tooling", "improve my dev workflow", or mentions project architecture, tooling choices, or Claude Code configuration. Also use when the working directory appears empty or newly created.
allowed-tools: Bash(cat:*), Bash(grep:*), Bash(sed:*), Bash(test:*), Bash(mkdir:*), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/best-practices-status.sh:*), Read, Write
---

# Best Practices

## Status

!`${CLAUDE_PLUGIN_ROOT}/scripts/best-practices-status.sh`

## Previously Rejected

@.claude/mats/best-practices-rejected.md

## Instructions

### First-Time Setup

Walk through all recommendations as a guided setup. For each category:
1. Check what's already implemented
2. Explain the recommendation
3. Offer to implement it
4. If rejected, note the reason for the rejected file

### Follow-Up Runs

1. Load the rejected items from the file above - respect previous decisions
2. Check what's currently implemented
3. Only show new or missing recommendations (skip rejected ones)
4. If plugin version changed, mention what's new

## Checklist

### Documentation

- [ ] **CLAUDE.md** - Project instructions for Claude
- [ ] **README.md** - Project documentation
- [ ] **@README.md in CLAUDE.md** - Living documentation pattern (Claude keeps README updated)

### Plugins (based on project type)

Check `.claude/settings.json` for installed plugins. Propose relevant ones:

- **MATS/Alignment**: `hive-mind@alignment-hive` - **Always ask about this one**
- **Python**: `pyright-lsp`
- **TypeScript/JavaScript**: `typescript-lsp`, `frontend-design` (for web projects)
- **Rust**: `rust-analyzer-lsp`
- **Agent development**: `agent-sdk-dev`

Install by adding to `./.claude/settings.json` (project root):

```json
{
  "enabledPlugins": {
    "pyright-lsp@claude-plugins-official": true
  }
}
```

For hive-mind (requires alignment-hive marketplace):
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

After installing plugins, tell the user to exit (`/exit`) and continue with `claude -c`.
For hive-mind, tell them to run `/hive-mind:setup` after restarting.

### Tooling (varies by project)

Consider modern tooling where appropriate:
- **Python**: `uv` for dependency management
- **JavaScript/TypeScript**: `vite`, `bun`
- **General**: linters, typecheckers, formatters

If a tool would be useful and isn't installed, ask if the user would like to install it.

## Guidance by Project Type

### New Projects

Spend the first session on architecture, research, and tooling:
- Make high-level architecture decisions
- Research existing solutions before building from scratch
- Set up the development environment

### Existing Projects

Focus on understanding and helping:
- Understand the current structure
- Suggest relevant plugins
- Help with whatever task brought them here
- Don't push architecture changes unless requested

## hive-mind Note

**Always ask** if the user wants to install hive-mind - this is a key part of the alignment research workflow. It provides:
- Local memory retrieval across your sessions
- Shared knowledge from other researchers' Claude sessions
- Your sessions contribute back to the community

Sharing requires an invite (MATS fellows: check your email).

## Completion

Once all recommendations have been either implemented or explicitly rejected:

1. Write the plugin version (shown in Status above) to `.claude/mats/best-practices-version` (the directory is created by the hook)

2. Write/update `.claude/mats/best-practices-rejected.md` with natural language descriptions of rejected recommendations. Format:
   ```markdown
   # Rejected Best Practices

   - User prefers pip over uv for Python dependency management
   - No hive-mind - working on sensitive project
   - Declined pyright-lsp - already using mypy
   ```

   This captures any rejection in flexible natural language, including tooling suggestions not explicitly listed here.

3. If nothing was rejected, either leave the file empty or don't create it.
