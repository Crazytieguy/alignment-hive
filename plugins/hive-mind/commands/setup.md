---
description: Walk through hive-mind installation and authentication
disable-model-invocation: true
allowed-tools: Bash(which bun:*), Bash(bun ${CLAUDE_PLUGIN_ROOT}/cli.js:*)
---

# hive-mind Setup

Guide the user through setting up hive-mind for session sharing.

## Current Status

Output for `which bun`:
```
!`which bun || echo "not found"`
```

Output for `bun ${CLAUDE_PLUGIN_ROOT}/cli.js setup --status` (only if bun is installed):
```
!`which bun > /dev/null && bun ${CLAUDE_PLUGIN_ROOT}/cli.js setup --status || echo "bun not installed"`
```

## Step 1: Install Bun (if needed)

If bun is not installed, run:
```bash
curl -fsSL https://bun.sh/install | bash
```

## Step 2: Explain Privacy and Get Consent

Before running setup, explain what hive-mind does:

**The basics:**
- Your Claude Code sessions can be shared with the alignment research community
- There's a 24-hour review period before sessions are uploaded - you can exclude any session with `hive-mind exclude`
- The data is processed on the server and made available to other researchers' Claude agents for retrieval - no one can browse your sessions directly

**Tradeoffs of logging in vs not:**
- Logging in: your sessions contribute to shared knowledge, and you can retrieve insights from other researchers' sessions
- Not logging in: local session retrieval still works, no data shared externally, can set up later anytime

**Only** if the user has questions or concerns about their data, explain:

- **What gets extracted**: User messages, assistant responses, tool inputs/outputs, and thinking blocks from Claude Code sessions. Images, PDFs, and other non-text content are excluded.
- **Sanitization**: Before upload, sessions are scanned using gitleaks rules to detect and remove secrets like API keys, tokens, and credentials.
- **Review process**: Sessions wait 24 hours after extraction before becoming eligible, then another 10 minutes before upload starts. Run `hive-mind index --pending` to see what's pending, `hive-mind exclude <session-id>` to exclude specific sessions, or `hive-mind exclude --all` to exclude everything pending.
- **Local storage**: Auth tokens are stored in `~/.claude/hive-mind/auth.json` with restricted permissions (only your user can read).

If the user agrees to proceed, have them run this command **in a separate terminal window**:

Output for `echo "bun ${CLAUDE_PLUGIN_ROOT}/cli.js setup"`:
```
!`echo "bun ${CLAUDE_PLUGIN_ROOT}/cli.js setup"`
```

This opens a browser for authentication. Wait for the user to complete setup before continuing.

## Step 3: Gitignore Decision

After setup completes, explain the gitignore tradeoff for `.claude/hive-mind/sessions/`:

**Not gitignoring (default):**
- Sessions sync when you push/pull the repo
- Works across different machines and for collaborators

**Gitignoring:**
- Keeps the git repo clean (sessions can add many files)
- Sessions stay only on the current machine
- Add to .gitignore: `.claude/hive-mind/sessions/`

Ask which the user prefers and help them configure it if they want to gitignore.
