---
description: Set up session sharing and get tooling recommendations — plugins and dev environment setup. This command should also be used when the user asks about setting up their project, which plugins to install, or when the working directory appears empty or newly created.
allowed-tools: Bash(hive consent status), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh)
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

### Data Sharing (check first, before recommendations)

Read the consent status output above. Handle errors first, then check if data sharing needs attention.

**If "error: binary not found"**: Direct user to run `curl -fsSL https://alignment-hive.com/install.sh | bash`, then restart Claude Code.

**If "Not authenticated"**: Direct user to run `curl -fsSL https://alignment-hive.com/install.sh | bash`.

**If "Data sharing preferences: not set"**: Direct user to `https://alignment-hive.com/consent`. Move on to recommendations.

**If "Session sharing: disabled"**: Note briefly that sharing is declined, changeable at `https://alignment-hive.com/consent`. Move on.

**If "Session sharing: enabled"**: Check whether data sharing state is settled or needs attention.

State is **settled** (note status in one line, move on) when:
- Current project is "enabled", AND either no `Repo visibility` line (not a GitHub repo), or repo is public, or repo is linked, or `.claude/hive/repo-linking-declined` exists
- OR `.claude/hive/sharing-disabled` exists (user previously declined for this project — mention they can re-enable with `/hive:align` or by asking to enable sharing)

State is **unsettled** (load the `manage-data-sharing` skill) when:
- Current project is "not enabled" and `.claude/hive/sharing-disabled` does not exist
- OR project is enabled with `Repo visibility: private` and `Repo link: not-linked` and `.claude/hive/repo-linking-declined` does not exist

### hive-mind Migration

If `hive-mind@alignment-hive` is in `.claude/settings.json` or `.claude/settings.local.json`, offer to remove it — hive handles session sharing now. If not found, skip silently — do not mention it.

### First-Time Setup (recommendations)

Walk through the checklist below. For each item, check if it's already implemented. If not, offer it to the user. Implement if accepted, note the reason in the rejected file if declined.

### Follow-Up Runs

1. Read the rejected items from the file above — respect previous decisions
2. Check what's currently implemented
3. Only show new or missing recommendations (skip rejected ones)
4. If plugin version changed, mention what's new

## Checklist

### Plugins (based on project type)

Check both global (`~/.claude/settings.json`, `~/.claude/settings.local.json`) and project-level (`.claude/settings.json`, `.claude/settings.local.json`) settings to discover already-installed plugins. A plugin installed at either level counts as installed — do not recommend it again.

**Always install plugins to project-level settings files** (`.claude/settings.json` or `.claude/settings.local.json` in the working directory) unless the user explicitly asks for a global install. Infer from existing project-level settings whether the user prefers local-only (`.claude/settings.local.json`) or shared (`.claude/settings.json`) — if unclear, ask once and use that for all installations.

Propose all relevant plugins in **batched AskUserQuestion calls**. Each plugin gets three options: **Yes** (install), **No** (skip), **Tell me more**. After the user responds, process "Tell me more" answers one plugin at a time in sequence: (1) fetch the full, untruncated content of that plugin's README from the table below (use curl — WebFetch summarizes), (2) present the full README content to the user verbatim (the READMEs are already concise — do not summarize or truncate), (3) ask a fresh AskUserQuestion with only **Yes** / **No**. Do not advance to the next "Tell me more" plugin until the current one has a Yes/No answer.

#### Plugin list

- **Autopilot** (permissions, autonomous mode, sandboxed scripting): `autopilot@alignment-hive` — **Always recommend**
- **GitHub Action**: `github-action@alignment-hive` — `@claude` mentions on issues/PRs for autonomous work
- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod)
- **Codebase exploration**: `precis` — Structural codebase summaries for fast agent context
- **TypeScript/JavaScript**: `frontend-design` (for web projects)

#### README URLs for "Tell me more"

| Plugin | README URL |
|---|---|
| autopilot | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/autopilot/README.md` |
| github-action | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/github-action/README.md` |
| mats | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/mats/README.md` |
| remote-kernels | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/remote-kernels/README.md` |
| precis | `https://raw.githubusercontent.com/Crazytieguy/precis/main/README.md` |
| frontend-design | `https://raw.githubusercontent.com/anthropics/claude-plugins-official/main/plugins/frontend-design/README.md` |

For non-alignment-hive plugins:
```json
{
  "enabledPlugins": {
    "precis@precis": true,
    "frontend-design@claude-plugins-official": true
  },
  "extraKnownMarketplaces": {
    "precis": {
      "source": {
        "source": "github",
        "repo": "Crazytieguy/precis"
      }
    }
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

After all plugins are installed, tell the user to exit and start a fresh Claude session (`/exit` then `claude`).

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
