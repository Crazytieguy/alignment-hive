---
description: Get tooling recommendations for your project. Use when the user asks about "best practices", "how should I set up", "what's the right way to", "help me get started", "start a project", "set up my environment", "which plugins should I install", "how to configure Claude Code", "optimize for Claude", "make my project work better with Claude", "project structure", "what tools should I use", "improve my tooling", "improve my dev workflow", or mentions project architecture, tooling choices, or Claude Code configuration. Also use when the working directory appears empty or newly created.
allowed-tools: Bash(cat:*), Bash(grep:*), Bash(sed:*), Bash(test:*), Bash(mkdir:*), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/recommendations-status.sh:*), Read, Write
---

# Recommendations

## Status

!`${CLAUDE_PLUGIN_ROOT}/scripts/recommendations-status.sh`

## Previously Rejected

@.claude/hive/recommendations-rejected.md

## Instructions

### First-Time Setup

Walk through all recommendations as a guided setup. For each category:
1. Check what's already implemented
2. Explain the recommendation
3. Offer to implement it
4. If rejected, note the reason for the rejected file

### Follow-Up Runs

1. Load the rejected items from the file above — respect previous decisions
2. Check what's currently implemented
3. Only show new or missing recommendations (skip rejected ones)
4. If plugin version changed, mention what's new

## Checklist

### Documentation

- [ ] **CLAUDE.md** — Project instructions for Claude
- [ ] **README.md** — Project documentation
- [ ] **@README.md in CLAUDE.md** — Living documentation pattern (Claude keeps README updated)

### Plugins (based on project type)

Check `.claude/settings.json` for installed plugins. Propose relevant ones:

- **Autopilot** (permissions + autonomous mode): `autopilot@alignment-hive` — **Always recommend**
- **GitHub Action**: `github-action@alignment-hive` — `@claude` mentions on issues/PRs for autonomous work
- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod)
- **Documentation fetching**: `llms-fetch-mcp@alignment-hive` — Fetch docs with [llms.txt](https://llmstxt.org/) support
- **TypeScript/JavaScript**: `frontend-design` (for web projects)

Install by adding to `./.claude/settings.json` (project root):

```json
{
  "enabledPlugins": {
    "frontend-design@claude-plugins-official": true
  }
}
```

For alignment-hive plugins (requires alignment-hive marketplace):
```json
{
  "enabledPlugins": {
    "autopilot@alignment-hive": true
  },
  "pluginMarketplaces": {
    "alignment-hive": "Crazytieguy/alignment-hive"
  }
}
```

If `pluginMarketplaces` doesn't include `alignment-hive`, tell the user to run the install script:
```
curl -fsSL https://alignment-hive.com/install.sh | bash
```

Do NOT invoke setup skills directly during this flow — just recommend installing the plugins. Setup flows will be triggered after reload.

### Tooling (varies by project)

Consider modern tooling where appropriate:
- **Python**: `uv` for dependency management
- **JavaScript/TypeScript**: `vite`, `bun`
- **General**: linters, typecheckers, formatters

If a tool would be useful and isn't installed, ask if the user would like to install it.

### Reload + Setup

After all plugins are installed, tell the user to exit and restart Claude (`/exit` then `claude -c`).

Tell the user which of the installed plugins have setup flows they should run after reloading. Mention by plugin name, not exact skill command:
- **autopilot** has a setup flow for permissions and autonomous mode
- **remote-kernels** has a setup flow for RunPod configuration

Each plugin's SessionStart hook will also nudge about its own setup when the session starts.

### GitHub Action (Async Claude)

- [ ] **GitHub Action workflows** — Enable `@claude` mentions on issues and PRs for autonomous work

**Detection:** Check for `.github/workflows/claude-issue.yml`.

**Action:** If the user agrees to set up the GitHub Action, invoke `/github-action:setup`. If the `github-action` plugin is not installed, tell the user to install it first: `/plugin install github-action@alignment-hive`.

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

## Completion

Once all recommendations have been either implemented or explicitly rejected:

1. Write the plugin version (shown in Status above) to `.claude/hive/recommendations-version` (the directory is created by the hook)

2. Write/update `.claude/hive/recommendations-rejected.md` with natural language descriptions of rejected recommendations. Format:
   ```markdown
   # Rejected Recommendations

   - User prefers pip over uv for Python dependency management
   - No GPU compute needed — working on theory
   - Declined pyright-lsp — already using mypy
   ```

   This captures any rejection in flexible natural language, including tooling suggestions not explicitly listed here.

3. If nothing was rejected, either leave the file empty or don't create it.
