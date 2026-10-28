# hive

Core plugin for alignment-hive. Installed automatically by the [install script](https://alignment-hive.com).

## What This Plugin Does

**Tooling recommendations** (`/hive:align`) — Walks through your project and recommends relevant plugins and dev tools. Tracks what you've already set up and what you've declined, so repeat runs only show new recommendations.

**Session sharing** — Opt-in system for sharing Claude Code session transcripts with AI safety research organizations. See [alignment-hive.com/policy](https://alignment-hive.com/policy) for what gets shared, who has access, and how to manage preferences. At the start of a session, rows above the prompt say what is about to upload, with **Review sessions** to preview it and **Snooze 24h** for an upload minutes away; once you're working, a row shows this session's state, with **Keep private** to leave it out. In the desktop app that row folds into a `hive:` label in the prompt footer after your first turn; click the label to open it again.

**Session retrieval** — Search past Claude Code sessions from your local machine. An agent automatically searches session history when past context might be relevant, or you can search manually. A PostToolUse hook on EnterWorktree/ExitWorktree registers the session's new transcript dir, so moved sessions stay searchable.

**Automatic recall (beta, opt-in)** — Runs an automatic memory recall step before sending each of your prompts, finding relevant context from this project's past sessions: your instructions and preferences, earlier decisions and their reasons, approaches that failed. Each prompt waits up to 30 seconds while one model call reads the project's recent messages and Claude's replies; Claude gets the entries that bear on the prompt, and can still spawn the retrieval agent for older history. With model-router it runs on GPT-6.1 Sol and uses none of your Claude usage. Without it, it runs on Sonnet, which finds less and adds about 5% to your Claude usage. A prompt recall found something for gets a dim `hive recalled N memories` line under it; click it to see the entries (with the classic terminal renderer, press ctrl+o). ctrl+o also says when recall found nothing or didn't finish. Only the running session's prompts show this; a resumed session's earlier prompts don't. `/hive:align` offers it; `/config` turns it on or off (`recall`) and picks the model (`recallModel`).

**CLI auto-updates** — Keeps the `hive` CLI binary up to date automatically at session start.
