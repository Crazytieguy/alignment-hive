---
description: Set up session sharing and get tooling recommendations — plugins and dev environment setup. This command should also be used when the user asks about setting up their project, which plugins to install, or when the working directory appears empty or newly created.
allowed-tools: Bash(hive consent status), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh)
---

# Align

## Status

!`${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh 2>/dev/null || echo "Status unavailable"`

## Consent Status

```
!`hive consent status 2>&1 || true`
```

## Previously Rejected

@.claude/hive/align-rejected.md

## Instructions

### Data Sharing (check first, before recommendations)

Read the consent status output above. Handle errors first, then check if data sharing needs attention.

**If the output looks like a shell error** (e.g. `command not found`, `No such file or directory`) rather than one of the statuses below: the `hive` binary is missing. Direct the user to run `curl -fsSL https://alignment-hive.com/install.sh | bash`, then restart Claude Code.

**If "Not authenticated"**: Not a problem — authentication is optional and only needed to opt into session data sharing. Mention in passing that running `curl -fsSL https://alignment-hive.com/install.sh | bash` enables it later if the user wants. Move on to recommendations.

**If "Failed to fetch consent status"**: Note briefly that sharing status couldn't be checked (offline or API issue). Move on to recommendations.

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

### autopilot Deprecation

If `autopilot@alignment-hive` is in `.claude/settings.json` or `.claude/settings.local.json`, note that Claude Code's built-in auto mode supersedes autopilot (except for the deno sandbox) — it uses a model-based classifier instead of a static allow-list.

Ask whether to remove `autopilot@alignment-hive`. Pro users (no auto mode access yet) or users who rely on the deno sandbox may want to keep it.

If the user chooses to keep it, record that in `.claude/hive/align-rejected.md` (e.g. "Kept autopilot") so we don't re-prompt on future runs.

If not installed, skip silently — do not mention it.

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

- **GitHub Action**: `github-action@alignment-hive` — `@claude` mentions on issues/PRs for autonomous work
- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod)
- **Codebase exploration**: `precis` — Structural codebase summaries for fast agent context
- **Cross-model review**: `codex@codex-plugin-cc` — Delegate tasks and adversarial code review to Codex from Claude Code
- **Cross-model subagents (experimental)**: `model-router@alignment-hive` — GPT models as native Claude Code subagents via a local gateway; experimental alternative to the codex plugin

#### README URLs for "Tell me more"

| Plugin | README URL |
|---|---|
| github-action | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/github-action/README.md` |
| mats | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/mats/README.md` |
| remote-kernels | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/remote-kernels/README.md` |
| precis | `https://raw.githubusercontent.com/Crazytieguy/precis/main/README.md` |
| codex | `https://raw.githubusercontent.com/Crazytieguy/codex-plugin-cc/main/README.md` |
| model-router | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/model-router/README.md` |

For non-alignment-hive plugins, write this block into the chosen settings file (shape is the same; values come from the table below):

```json
{
  "enabledPlugins": { "<plugin>": true },
  "extraKnownMarketplaces": {
    "<marketplace>": {
      "source": { "source": "github", "repo": "<github-repo>" },
      "autoUpdate": true
    }
  }
}
```

| Plugin | `<plugin>` (enabledPlugins key) | `<marketplace>` | `<github-repo>` |
|---|---|---|---|
| precis | `precis@precis` | `precis` | `Crazytieguy/precis` |
| codex | `codex@codex-plugin-cc` | `codex-plugin-cc` | `Crazytieguy/codex-plugin-cc` |

`autoUpdate: true` is included by default for recommended non-alignment-hive marketplaces — they iterate quickly and benefit from auto-refresh, and the user has already opted in by accepting the recommendation. Claude Code (v2.1.140+) propagates this field to `~/.claude/plugins/known_marketplaces.json` on next session start.

For alignment-hive plugins (requires alignment-hive marketplace):
```json
{
  "enabledPlugins": {
    "github-action@alignment-hive": true
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

#### Marketplace auto-update — retroactive sweep

Recommended non-alignment-hive marketplaces (hardcoded): `precis`, `codex-plugin-cc`. Update this list whenever the plugin list above changes.

Some users may have these marketplaces installed without `autoUpdate: true` (installed before this skill enabled it by default, or installed via the `/plugin` TUI). For each affected marketplace, ask once whether to enable auto-update; idempotent — skip anything already enabled or already declined.

1. Read all four settings files (`~/.claude/settings.json`, `~/.claude/settings.local.json`, `.claude/settings.json`, `.claude/settings.local.json`) and `~/.claude/plugins/known_marketplaces.json`.
2. For each settings file, record where each plugin is enabled (`enabledPlugins` keys with value `true` → extract `<plugin>@<marketplace>`) and where each marketplace is declared (`extraKnownMarketplaces.<marketplace>` and its `autoUpdate` value if any).
3. For each candidate marketplace where: name ∈ `{precis, codex-plugin-cc}` AND at least one plugin from it is enabled in some settings file AND `autoUpdate` is not already `true` in **any** settings file's `extraKnownMarketplaces.<marketplace>.autoUpdate` AND `autoUpdate` is not already `true` in the registry entry AND not already recorded as declined in `.claude/hive/align-rejected.md` → include in the ask.
4. Ask via `AskUserQuestion` using the same pattern as the plugin recommendations above: one `Question` per candidate marketplace, two options each (Yes / No), all questions batched in a single tool call.
5. For Yes answers, determine the target settings file by walking this preference order until a match is found, then edit that file:
   - The file that already declares the marketplace in its `extraKnownMarketplaces` (add `"autoUpdate": true` to the existing entry, preserve other fields). If multiple files declare it, prefer the most local: `.claude/settings.local.json` > `.claude/settings.json` > `~/.claude/settings.local.json` > `~/.claude/settings.json`.
   - Otherwise, the file that enables the plugin (add a full `extraKnownMarketplaces.<marketplace>` entry with the source from the install mapping table above + `"autoUpdate": true`). Same preference order if multiple files enable it.

   This keeps personal vs shared state aligned with the user's existing choices and never promotes a third-party marketplace declaration into a more-shared file than the user already chose for the plugin itself. The change takes effect on the next session start (Claude Code propagates `autoUpdate` to the registry then).
6. For No answers: append to `.claude/hive/align-rejected.md` (e.g. "Declined auto-update for `precis` marketplace") so we don't re-prompt.

Skip the sweep entirely if no candidates remain after filtering.

#### alignment-hive auto-update verification

Read the `alignment-hive` entry in `~/.claude/plugins/known_marketplaces.json`. If `autoUpdate` is not `true`, mention once that the install script (`curl -fsSL https://alignment-hive.com/install.sh | bash`) is supposed to set this and recommend re-running it. Do not auto-fix.

### Tooling (varies by project)

Use your judgement to recommend modern, well-maintained tooling appropriate for the project. Consider dependency management, build tools, linters, typecheckers, formatters, and anything else that would improve the development workflow.

If a tool would be useful and isn't installed, ask if the user would like to install it.

### Reload + Setup

Mention that some plugins have setup skills that will be available after reloading — each plugin's SessionStart hook will nudge about its own setup when the session starts. Also mention which marketplaces just had auto-update enabled, so the user understands those will refresh automatically on session start.

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
