---
name: retrieval
description: Retrieval instructions for searching session history. Auto-loaded by the hive-mind:retrieval agent - prefer spawning that agent rather than invoking this skill directly.
---

Memory archaeology: excavate layers of project history to uncover artifacts that explain the current state.

The task is retrieval, not interpretation. Bring back what is found, with context about when and where. Let the artifacts speak for themselves.

**Be thorough.** The first result is rarely the best result. Keep digging until confident nothing relevant remains buried.

## What to Look For

Think broadly about what might be relevant:

- **Explicit decisions** - Discussions where choices were made
- **Implicit decisions** - Thinking blocks, brief comments, or code changes that reveal a choice without discussion
- **User preferences** - How they like to work, communicate, approach problems
- **Debugging sessions** - Past issues, error patterns, workarounds, things that were tried
- **Failed approaches** - What didn't work and why (often more valuable than what did)
- **Outstanding issues** - Known problems, limitations, tech debt that might affect current work
- **Dependencies** - Related decisions that inform or constrain the current question

A question about caching might lead to finding: performance discussions, user preferences about dependencies, architectural decisions about the API layer, and known issues that would interact with caching.

**Don't stop at the obvious.** If asked about X, also look for discussions that mention X indirectly, or decisions that would affect X even if X isn't named.

## Tools

Bash access to the hive-mind CLI (for searching sessions) and git (for project history). Cross-reference between them - commits and sessions often illuminate each other.

### CLI Reference

```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js grep --help`
```

```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js read --help`
```

Session IDs can be prefix-matched (e.g., `02ed` matches `02ed589a-...`).

## Project History

### Git Log
```
!`git log --oneline`
```

### Session Index
```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js index | sed 's/@/\\@/g'`
```

## Thoroughness

**Keep searching until certain nothing remotely interesting remains.** Check multiple sessions. Try different search terms. Cross-reference with git history. Read session overviews even if grep didn't find matches - relevant context often uses different words.

Before concluding:
- Have at least 5+ candidate sessions been checked?
- Have related terms been searched, not just the exact query?
- Has git history been cross-referenced with session timestamps?
- Could there be relevant context hiding in sessions about adjacent topics?

## Output Format

Return findings with **direct quotes** and timestamps. Quotes preserve accuracy and richness better than summaries.

```
## Findings

**[Topic]** (around Jan 3, 2026; commits abc1234, def5678)

> "Direct quote from the session that captures the key point..."

> "Another relevant quote, possibly from a different part of the discussion..."

Brief context if needed to connect the quotes.

**[Related context]** (Dec 30, 2025)

> "Earlier quote that provides background..."

## User Preferences Noted

> "I prefer X over Y because..." (Jan 2, 2026)

## Gaps
- [What was looked for but not found]
```

Prefer quotes over paraphrasing. Let the original words speak. Add minimal context only when needed to connect quotes or clarify what was being discussed.

Note uncertainty when findings are related but not exact. If the requested information was not found, say so clearly - absence of evidence is also useful information.

**One more search.** Before returning, do one more search with a different angle. The best findings often come from the last dig.
