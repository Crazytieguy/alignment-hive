---
name: consent-setup
description: This skill should be used when a user's consent state is unsettled — session sharing is enabled globally but the current project isn't enabled yet, or the project is enabled but a private repo isn't linked for code context. Auto-loaded by /hive:align when action is needed.
allowed-tools: Bash(hive consent enable:*), Bash(hive consent disable:*), Bash(hive consent status:*), Read, Write
---

# Consent Setup

Handle unsettled consent states for the current project during `/hive:align`. The align command loads this skill only when the user needs to make a decision.

## Session Sharing

When sharing is globally enabled but not yet enabled for the current project:

1. Explain what session sharing means: sessions from this project will be shared with the alignment research community after a 24-hour review period.
2. Use AskUserQuestion to offer enabling.
3. If accepted: run `hive consent enable` (user approves via Claude Code permission prompt). After enabling, briefly mention:
   - Sessions have a 24h review period before upload
   - `hive upload --help` shows available commands
   - `hive upload review` opens a local web UI to preview uploads
4. If declined: write "Declined session sharing" to `.claude/hive/align-rejected.md` and create `.claude/hive/sharing-disabled` marker.

## Repo Linking for Code Context

After session sharing is settled (either just enabled or was already enabled), check the output of `hive consent status` for the "Repo visibility" and "Repo link" lines.

**When to prompt:** Only if all of these are true:
- The project has a GitHub remote
- `Repo visibility: private`
- `Repo link: not-linked`
- `.claude/hive/repo-linking-declined` does NOT exist in the project's state dir

**What to explain:** Installing the GitHub App lets data consumers see the code at commits referenced in sessions. This provides full context for shared work. It grants read access to selected repositories — the user chooses which repos on GitHub's install page.

**How to offer:**
- Provide the install link: `https://github.com/apps/alignment-hive/installations/new` (or point to `https://github.com/settings/installations` if they want to manage an existing installation)
- If accepted: open the URL for the user
- If declined: create `.claude/hive/repo-linking-declined` in the project's state dir

**Skip silently when:**
- `Repo visibility: public` — code context works automatically for public repos
- `Repo link: linked` — already set up
- `Repo visibility` line is absent — not a GitHub repo
- `.claude/hive/repo-linking-declined` exists — user previously declined
