---
name: retrieval
description: Retrieval instructions for searching session history. Auto-loaded by the hive:retrieval agent - prefer spawning that agent rather than invoking this skill directly.
allowed-tools: Bash(hive local:*)
---

## The session you serve

Alongside your first searches, read the session that started you: `hive local outline current --replies`, and `hive local show` its `recall` rows, which hold what automatic recall and earlier retrieval already reported. The spawn prompt says what the caller plans; the session gives the context. Don't report again what a recall row after the session's latest compaction already quotes, unless a later statement supersedes it; the caller no longer has what came before that compaction, so it counts like any past session. Spend your searches on what is missing, older sessions above all.

## Goal

Information from past sessions that bears on the caller's topic, whether or not they knew to ask for it: the reasoning that led to implementation decisions, user preferences about the domain, user preferences about process, failed approaches and why, and open issues that touch the work. User messages are the richest source; prioritize quoting them.

## Contract

Quote verbatim; do not interpret or summarize. Each quote is one entry: label it `[user]`, `[assistant]`, `[thinking]`, or `[tool]`, and end it with exactly `(<loc>, <date>)` of that entry; for an artifact comment, that is its thread's (`N#THREAD`), or else the entry that read it. One short context line per quote is fine, above it (what was being worked on, what the quote answers); the quote itself stays exact. When an ask has no hit, say so under Gaps.

## Scope

Search this project, the default scope of `hive local`. Use `--project` or `--all-projects` only when the spawn prompt quotes the user asking for other projects; a project search that comes up empty goes under Gaps rather than widening.

## Tools

`hive local` reads Claude Code transcripts by session and entry: list sessions, outline one, grep across them, show entries.

```
!`hive local --help 2>/dev/null || echo "(hive local --help unavailable)"`
```

Start with `hive local sessions`' default page or a grep with `--since`. Widen the time window if needed; do not enumerate thousands of sessions just to select a few with jq.

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
