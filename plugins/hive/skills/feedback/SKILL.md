---
name: feedback
description: File a GitHub issue about alignment-hive, with a bug report, feature request or other feedback about any alignment-hive plugin (hive, debrief, model-router, remote-kernels, lit-review, tldr, llms-fetch-mcp, autopilot, github-action), the hive CLI, or alignment-hive.com. Use when the user wants to report or suggest something about them, and when you run into what looks like a bug in one of them yourself.
argument-hint: "[what to report]"
allowed-tools: Bash(gh search issues:*), Bash(gh issue list:*), Bash(gh issue view:*), Bash(claude --version), Bash(claude plugin list:*), Bash(hive --version), Bash(uname:*)
---

# Feedback

File this as an issue on `Crazytieguy/alignment-hive`, not the user's own repo: $ARGUMENTS

If that is empty, the feedback is whatever prompted this in the conversation. Whatever the user asks for explicitly goes, even where it differs from the defaults below.

## Writing it

It has two readers: a human maintainer with little context, who should be able to tell quickly what is wrong or wanted, and the agent that works on it, which needs the full details. So keep the top short and put the details under it.

- The title states the problem or the wish in plain words, prefixed with the part it concerns like the repo's existing issues (`hive: ...`, `model-router: ...`).
- A bug: what happened and what was expected, in terms someone could see for themselves. A request: what the user was trying to get done, then what would meet it; the need matters more than the proposed fix, if any.
- Say how much it matters (how often it happens, any workaround), with numbers when you have them.
- Keep what you saw apart from what you guess, and say plainly what you didn't verify.
- One topic per issue. Nothing about how the session went or who noticed what.
- End with the line `Filed with /hive:feedback`.

For a bug, gather as much telemetry as you can yourself rather than asking the user, and put it in a collapsed `<details>` block under the short description:

- Steps that reproduce it, or the closest you have.
- Exact error text, and the relevant lines of logs or hook output.
- Versions: `claude --version`, the alignment-hive plugins in `claude plugin list`, `hive --version` when the CLI is involved, the OS and architecture, and the surface (terminal, desktop app, headless) when it could matter.
- The cause, if you found it. Installed plugin source is under `plugins/cache/` in the Claude Code config dir (`~/.claude` unless `CLAUDE_CONFIG_DIR` is set).

For a long or subtle issue, an optional check: give a subagent only the draft, and ask whether it can tell what is wrong or wanted, and what is confusing, redundant, missing, or reveals something about the user or their project. Skip it for small ones.

## Posting

The issue is public and filed from the user's GitHub account. One that reveals nothing about the user or their project can be posted without asking. Watch what hides in telemetry: names, usernames in home paths (write `~`), the user's repo and file names, their code, data and prompts, hostnames, keys. Generalize or cut those unless the user asks to keep them, and if you're not sure something is fine to share, ask.

Check for an existing issue first; if one already covers it, comment there only if you have something to add. After filing, give the user the link in one line, and if you filed it in the middle of other work, get back to that work.

Without a working `gh`, give the user a prefilled link, `https://github.com/Crazytieguy/alignment-hive/issues/new?title=...&body=...` with both values URL-encoded, or for a long body the title and body to paste there.
