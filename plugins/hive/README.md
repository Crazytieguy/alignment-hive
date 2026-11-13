# hive

Core plugin for alignment-hive. Installed automatically by the [install script](https://alignment-hive.com).

## What This Plugin Does

**Tooling recommendations** (`/hive:align`) — Walks through your project and recommends relevant plugins and dev tools. Tracks what you've already set up and what you've declined, so repeat runs only show new recommendations.

**Session sharing** — Opt-in system for sharing Claude Code session transcripts with AI safety research organizations. See [alignment-hive.com/policy](https://alignment-hive.com/policy) for what gets shared, who has access, and how to manage preferences. At the start of a session, rows above the prompt say what is about to upload, with **Review sessions** to preview it and **Snooze 24h** for an upload minutes away; once you're working, a row shows this session's state, with **Keep private** to leave it out. In the desktop app that row folds into a `hive:` label in the prompt footer after your first turn; click the label to open it again.

**Session retrieval** — Search past Claude Code sessions from your local machine. An agent automatically searches session history when past context might be relevant, or you can search manually. A PostToolUse hook on EnterWorktree/ExitWorktree registers the session's new transcript dir, so moved sessions stay searchable.

**Recall (beta, opt-in)** — A tool Claude calls as it works to find relevant context from this project's past sessions: your instructions and preferences, earlier decisions and their reasons, approaches that failed. Each call is one model call of up to 30 seconds over the project's recent messages and Claude's replies: on GPT-6.2 Sol with model-router, using none of your Claude usage, otherwise on Sonnet, which finds less and adds to your Claude usage. A background `hive` process keeps recent history in memory for it, and exits after an hour unused. While recall is on, Claude doesn't write memory files or CLAUDE.md for what you tell it unless you name the place, and says in its replies what a later session should know. `/remember <note>` saves a note for later sessions without starting a turn. `/hive:align` offers recall, then offers to turn off Claude Code's own memory and carry over the memories worth keeping. `/config` also picks the model (`recallModel`) and can turn on automatic recall (`automaticRecall`), which runs recall before each prompt you send as well.

**Feedback** (`/hive:feedback`) — Files a bug report or request about any alignment-hive plugin as an issue on this repo, from your GitHub account. Claude may also file one on its own when it hits a bug in a plugin, and asks first when the issue would reveal anything about you or your project.

**CLI auto-updates** — Keeps the `hive` CLI binary up to date automatically at session start.
