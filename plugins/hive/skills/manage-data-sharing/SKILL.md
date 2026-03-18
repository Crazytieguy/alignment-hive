---
name: manage-data-sharing
description: This skill should be used when the user asks to "enable sharing", "disable sharing", "stop sharing", "manage data sharing", "change sharing settings", "data sharing preferences", "consent settings", "opt out", "sharing status", "what am I sharing", or when data sharing state needs attention for the current project during /hive:align.
---

# Manage Data Sharing

Guide the user through alignment-hive data sharing settings. Handle both first-time setup and ongoing adjustments.

For the full data sharing policy (what gets shared, who has access, deletion): `https://alignment-hive.com/consent`

In brief: sessions are conversation transcripts (prompts, responses, tool calls, file paths, code snippets). Images and PDFs are stripped. Automated secret removal runs before upload. Data is shared with a curated group of AI safety researchers listed on the consent page.

## Flow

If loaded from `/hive:align`, the consent status output is already available above. Otherwise, run `hive consent status` to check state. Walk through the steps below in order, skipping steps that don't apply.

### Step 1: Enable project sharing

**When:** `Session sharing: enabled` but `Current project: not enabled`, and `.claude/hive/sharing-disabled` does not exist.

**Ask:** "Sessions from this project would be shared with the alignment research community. Enable sharing for this project?"

Provide the context needed to decide: sessions go through a 24-hour review period before upload. During that window, the user can review or exclude any session.

- If accepted: run `hive consent enable`.
- If declined: run `mkdir -p .claude/hive && echo '' > .claude/hive/sharing-disabled`.

### Step 2: Disable project sharing

**When:** The user explicitly asks to stop or disable sharing for this project.

**Ask:** "Disable session sharing for this project? Previously uploaded sessions are not automatically deleted."

- If confirmed: run `hive consent disable`.

### Step 3: Grant repo access

**When:** Project sharing is enabled, the project has a GitHub remote, `Repo visibility: private`, `Repo link: not-linked`, and `.claude/hive/repo-linking-declined` does not exist.

**Skip silently when:** repo is public, already linked, not on GitHub, no `Repo visibility` line in status output, visibility or link status is `unknown`, or `.claude/hive/repo-linking-declined` exists.

**Ask:** "This is a private repo. Would you like to grant repo access so researchers can see the code your sessions reference?"

Provide the context needed to decide: this grants read access to all files and history in the repositories selected on GitHub's page. The user chooses which repos and can revoke access anytime.

- If accepted: provide `https://github.com/apps/alignment-hive/installations/new`. Mention existing access can be managed at `https://github.com/settings/installations`.
- If declined: run `mkdir -p .claude/hive && echo '' > .claude/hive/repo-linking-declined`.

### Step 4: Summary

After all steps, briefly summarize what was set up. If sharing was enabled, mention:
- `hive upload review` opens a local web UI to preview pending uploads
- `hive upload --help` shows available upload commands
- Full data sharing policy: `https://alignment-hive.com/consent`
