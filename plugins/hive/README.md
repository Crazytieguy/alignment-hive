# hive

Core plugin for alignment-hive. Installed automatically by the [install script](https://alignment-hive.com).

## What This Plugin Does

**Tooling recommendations** (`/hive:align`) — Walks through your project and recommends relevant plugins and dev tools. Tracks what you've already set up and what you've declined, so repeat runs only show new recommendations.

**Session sharing** — Opt-in system for sharing Claude Code session transcripts with AI safety research organizations. See [alignment-hive.com/policy](https://alignment-hive.com/policy) for what gets shared, who has access, and how to manage preferences. At the start of a session, rows above the prompt say what is about to upload, with **Review sessions** to preview it and **Snooze 24h** for an upload minutes away; once you're working, a row shows this session's state, with **Keep private** to leave it out.

**Session retrieval** — Search past Claude Code sessions from your local machine. An agent automatically searches session history when past context might be relevant, or you can search manually. A PostToolUse hook on EnterWorktree/ExitWorktree registers the session's new transcript dir, so moved sessions stay searchable.

**CLI auto-updates** — Keeps the `hive` CLI binary up to date automatically at session start.
