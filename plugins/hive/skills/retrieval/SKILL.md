---
name: retrieval
description: Retrieval instructions for searching session history. Auto-loaded by the hive:retrieval agent - prefer spawning that agent rather than invoking this skill directly.
allowed-tools: Bash(hive local:*)
---

## Goal

Information from past sessions that bears on the caller's topic, whether or not they knew to ask for it: the reasoning that led to implementation decisions, user preferences about the domain, user preferences about process, failed approaches and why, and open issues that touch the work. User messages are the richest source; prioritize quoting them.

## Contract

Quote verbatim; do not interpret or summarize. Label each quote `[user]`, `[assistant]`, `[thinking]`, or `[tool]` with its `loc` and date. One short context line per quote is fine (what was being worked on, what the quote answers); the quote itself stays exact. When an ask has no hit, say so under Gaps.

## Scope

Search this project, the default scope of `hive local`. Use `--project` or `--all-projects` only when the spawn prompt quotes the user asking for other projects; a project search that comes up empty goes under Gaps rather than widening.

## Tools

`hive local` reads Claude Code transcripts by session and entry: list sessions, outline one, grep across them, show entries.

```
!`hive local --help 2>/dev/null || echo "(hive local --help unavailable)"`
```

To find the latest entries across sessions, merge by time and stop at a session whose end is older than what you need.

Use git commands if they help, but past sessions are the primary source.

## Output

```markdown
## Findings

<Context line, optional>
> [<source>] <verbatim quote> (<loc>, <date>)

## User Preferences Noted

> [user] <verbatim quote> (<loc>, <date>)

## Gaps
- <Ask without a hit>
```
