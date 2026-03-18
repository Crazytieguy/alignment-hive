---
name: align
description: Set up session sharing and get tooling recommendations — plugins, documentation patterns, and dev environment setup. This command should also be used when the user asks about setting up their project, which plugins to install, or when the working directory appears empty or newly created.
allowed-tools: Bash(cat:*), Bash(grep:*), Bash(sed:*), Bash(test:*), Bash(mkdir:*), Bash(hive consent status:*), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh:*), Read, Write
---

# Align

## Status

!`${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh 2>/dev/null || echo "Status unavailable"`

## Consent Status

```
!`hive consent status 2>/dev/null || echo 'error: binary not found'`
```

## Previously Rejected

@.claude/hive/align-rejected.md

## Instructions

### Session Sharing (ask first, before recommendations)

Read the consent status output above to determine the user's state.

**If "error: binary not found"**:
The hive binary is not installed. Direct the user to run the install script:
`curl -fsSL https://alignment-hive.com/install.sh | bash`
Then restart Claude Code and run /hive:align again.

**If "Not authenticated"**:
Direct the user to run the install script to authenticate:
`curl -fsSL https://alignment-hive.com/install.sh | bash`

**If "Web consent: not completed"**:
Direct the user to: `https://alignment-hive.com/consent`
Do not offer per-project sharing until web consent is complete. Move on to recommendations.

**If "Session sharing: disabled"**:
Note: "Session sharing: declined on web. You can change this at https://alignment-hive.com/consent"
Do not offer per-project sharing. Move on to recommendations.

**If "Session sharing: enabled"**:
Check the current project line.

- **If current project is "enabled"**: Note "Session sharing: enabled" and move on.
- **If current project is "not enabled" and `.claude/hive/sharing-disabled` exists**: The user previously declined sharing for this project. Note this and move on. Mention they can re-enable anytime with /hive:align.
- **If current project is "not enabled" and no sharing-disabled file**: Offer to enable sharing:
  - Explain this will share sessions from this project after a 24-hour review period
  - If they accept: run `hive consent enable` (user will see and approve via Claude Code permission prompt). The command handles everything including clearing any previous opt-out markers.
  - After enabling, briefly explain:
    - Sessions have a 24h review period before upload
    - `hive upload --help` shows all available commands for managing uploads
    - `hive upload review` opens a local web UI to preview what will be uploaded
  - If they decline: record "Declined session sharing" in `.claude/hive/align-rejected.md`
  - Repo linking for private repos is coming soon — mention this briefly if the project has a git remote

### hive-mind Migration

If `hive-mind@alignment-hive` is in `.claude/settings.json` or `.claude/settings.local.json`, offer to remove it — hive handles session sharing now.

### First-Time Setup (recommendations)

Walk through recommendations one at a time. For each item, check if it's already implemented. If not, use AskUserQuestion to offer it. Implement if accepted, note the reason in the rejected file if declined.

### Follow-Up Runs

1. Read the rejected items from the file above — respect previous decisions
2. Check what's currently implemented
3. Only show new or missing recommendations (skip rejected ones)
4. If plugin version changed, mention what's new

## Checklist

### Documentation

- [ ] **CLAUDE.md** — Project instructions for Claude
- [ ] **README.md** — Project documentation
- [ ] **@README.md in CLAUDE.md** — Living documentation pattern (Claude keeps README updated)

### Plugins (based on project type)

Check `.claude/settings.json` and `.claude/settings.local.json` for installed plugins. Infer from existing settings whether the user prefers local-only (`.claude/settings.local.json`) or shared (`.claude/settings.json`) — if unclear, ask once and use that for all installations.

Propose relevant plugins:

- **Autopilot** (permissions + autonomous mode): `autopilot@alignment-hive` — **Always recommend**
- **GitHub Action**: `github-action@alignment-hive` — `@claude` mentions on issues/PRs for autonomous work
- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod)
- **Documentation fetching**: `llms-fetch-mcp@alignment-hive` — Fetch docs with [llms.txt](https://llmstxt.org/) support
- **TypeScript/JavaScript**: `frontend-design` (for web projects)

For non-alignment-hive plugins:
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
  "extraKnownMarketplaces": {
    "alignment-hive": {
      "source": {
        "source": "github",
        "repo": "Crazytieguy/alignment-hive"
      }
    }
  }
}
```

### Tooling (varies by project)

Use your judgement to recommend modern, well-maintained tooling appropriate for the project. Consider dependency management, build tools, linters, typecheckers, formatters, and anything else that would improve the development workflow.

If a tool would be useful and isn't installed, ask if the user would like to install it.

### Reload + Setup

Mention that some plugins have setup skills that will be available after reloading — each plugin's SessionStart hook will nudge about its own setup when the session starts.

After all plugins are installed, tell the user to exit and restart Claude (`/exit` then `claude -c`).

## Completion

Once all recommendations have been either implemented or explicitly rejected:

1. Write the plugin version (shown in Status above) to `.claude/hive/align-version` (the directory is created by the hook)

2. Write/update `.claude/hive/align-rejected.md` with natural language descriptions of rejected recommendations. Format:
   ```markdown
   # Rejected Recommendations

   - User prefers pip over uv for Python dependency management
   - No GPU compute needed
   - Declined GitHub Action
   ```

   This captures any rejection in flexible natural language, including tooling suggestions not explicitly listed here.

3. If nothing was rejected, don't create the file.
